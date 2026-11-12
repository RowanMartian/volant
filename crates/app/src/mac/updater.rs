//! Updates in place.
//!
//! Every GitHub build of Volant knows the commit it was built from. The updater
//! asks GitHub which commit the "Latest build" release was made from; if it's a
//! different one, an update is offered. Installing downloads the zip, checks it
//! against the SHA-256 GitHub publishes for it, unpacks it next to the running
//! app (all in Rust: volant-core's `hash` and `zip`), checks it really is
//! Volant, then Volant itself (`--finish-update`) swaps the two app bundles the
//! moment the old one quits and opens the new one. Your tabs, history and
//! settings live outside the app and stay.

use super::{later, on_main, pages, ui, web, with, Browser, COMMIT};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSWorkspace, NSWorkspaceOpenConfiguration};
use objc2_foundation::{NSBundle, NSURL};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use volant_core::{hash, json, zip};

pub const REPO: &str = match option_env!("VOLANT_REPO") {
    Some(r) => r,
    None => "RowanMartian/volant",
};
const BUNDLE_ID: &str = "app.volant.browser";
/// How often to look for updates in the background.
const EVERY: u64 = 6 * 3600;

#[derive(Default)]
pub struct State {
    /// The commit of a newer build, if there is one.
    pub available: Option<String>,
    pub checking: bool,
    pub installing: bool,
    pub last_check: u64,
}

fn releases_page() -> String {
    format!("https://github.com/{REPO}/releases/tag/latest")
}

/// Tells the Settings page what the updater is doing.
fn status(b: &Browser, what: &str) {
    pages::notify(b, &format!("window.volantUpdate && volantUpdate({})", json::string(what)));
}

pub fn state_word(b: &Browser) -> &'static str {
    if COMMIT.is_none() {
        "dev"
    } else if b.update.installing {
        "installing"
    } else if b.update.checking {
        "checking"
    } else if b.update.available.is_some() {
        "available"
    } else if b.update.last_check > 0 {
        "current"
    } else {
        ""
    }
}

/// The first check shortly after launch; later ones ride the minute timer.
pub fn schedule(_b: &Browser) {
    if COMMIT.is_some() {
        super::after(8.0, || with(|b| check(b, false)));
    }
}

/// Called from the housekeeping timer.
pub fn tick(b: &mut Browser) {
    if COMMIT.is_some() && b.update.available.is_none() && super::now().saturating_sub(b.update.last_check) > EVERY {
        check(b, false);
    }
}

/// Asks GitHub for the latest build. `manual` means the person asked, so say how it went.
pub fn check(b: &mut Browser, manual: bool) {
    let Some(mine) = COMMIT.map(str::to_string) else {
        if manual {
            later(|| {
                web::alert("This is a development build", "Builds made by GitHub update themselves. Builds you make yourself don't.", &["OK"]);
            });
        }
        return;
    };
    if b.update.checking || b.update.installing {
        return;
    }
    b.update.checking = true;
    status(b, "checking");
    std::thread::spawn(move || {
        let latest = release().ok().map(|r| r.commit);
        on_main(move || {
            with(move |b| {
                b.update.checking = false;
                b.update.last_check = super::now();
                match latest {
                    Some(sha) if sha.len() >= 7 && !sha.starts_with(&mine) && !mine.starts_with(&sha) => {
                        b.update.available = Some(sha);
                        status(b, "available");
                        ui::update_downloads(b);
                        if manual {
                            later(|| offer());
                        }
                    }
                    Some(_) => {
                        b.update.available = None;
                        status(b, "current");
                        ui::update_downloads(b);
                        if manual {
                            later(|| {
                                web::alert("Volant is up to date", "You have the latest build.", &["OK"]);
                            });
                        }
                    }
                    None => {
                        status(b, "error");
                        if manual {
                            later(|| {
                                web::alert("Couldn't check for updates", "GitHub didn't answer. Check your connection and try again.", &["OK"]);
                            });
                        }
                    }
                }
            })
        });
    });
}

/// "Update and restart?" — asked outside any state borrow, since alerts run their own event loop.
pub fn offer() {
    if web::alert("A new version of Volant is ready", "Volant will restart to update. Your tabs will be right where you left them.", &["Update and Restart", "Later"]) == 0 {
        with(install);
    }
}

/// The "Latest build" release, as GitHub describes it.
struct Release {
    commit: String,
    zip_url: String,
    /// The SHA-256 GitHub computed for the zip when it was uploaded.
    zip_sha256: Option<String>,
}

fn release() -> Result<Release, String> {
    let api = format!("https://api.github.com/repos/{REPO}/releases/tags/latest");
    let mut opts = super::net::options();
    opts.headers.push(("Accept".into(), "application/vnd.github+json".into()));
    // CI hands over its token, as shared runners exhaust the anonymous rate limit.
    if let Ok(t) = std::env::var("VOLANT_GITHUB_TOKEN") {
        if !t.is_empty() {
            opts.headers.push(("Authorization".into(), format!("Bearer {t}")));
        }
    }
    opts.limit = 4 << 20;
    let r = super::net::get(&api, &opts)?;
    if !r.ok() {
        return Err(format!("GitHub answered {}", r.status));
    }
    let v = json::parse(&r.text()).ok_or("GitHub's answer wasn't JSON")?;
    let commit = v.str_at("target_commitish").ok_or("no commit in GitHub's answer")?.to_string();
    let asset = v.get("assets").map(json::Value::as_array).unwrap_or(&[]).iter().find(|a| a.str_at("name") == Some("Volant-mac.zip"));
    let zip_url = asset
        .and_then(|a| a.str_at("browser_download_url"))
        .map(String::from)
        .unwrap_or_else(|| format!("https://github.com/{REPO}/releases/download/latest/Volant-mac.zip"));
    let zip_sha256 = asset.and_then(|a| a.str_at("digest")).and_then(|d| d.strip_prefix("sha256:")).map(str::to_ascii_lowercase);
    Ok(Release { commit, zip_url, zip_sha256 })
}

fn app_path() -> Option<PathBuf> {
    let p = PathBuf::from(NSBundle::mainBundle().bundlePath().to_string());
    (p.extension().map(|e| e == "app").unwrap_or(false)).then_some(p)
}

pub fn install(b: &mut Browser) {
    if b.update.installing {
        return;
    }
    let Some(app) = app_path() else {
        super::actions::open_tab(b, &releases_page(), true);
        return;
    };
    let Some(parent) = app.parent().map(Path::to_path_buf) else { return };
    let staging = parent.join(format!(".volant-update-{}", std::process::id()));
    // Not allowed to write next to the app (a non-admin account): let them download it instead.
    if std::fs::create_dir_all(&staging).is_err() {
        super::actions::open_tab(b, &releases_page(), true);
        return;
    }
    b.update.installing = true;
    status(b, "installing");
    ui::update_downloads(b);
    std::thread::spawn(move || {
        let result = download_and_check(&staging);
        on_main(move || {
            with(move |b| match result {
                Ok(new_app) => {
                    // Save the tabs now; quitting happens after this borrow ends.
                    b.session_dirty = true;
                    super::actions::save_session(b);
                    later(move || swap_and_relaunch(&app, &new_app, &staging));
                }
                Err(why) => {
                    eprintln!("update failed: {why}");
                    if std::env::args().any(|a| a == "--install-update") {
                        std::process::exit(1);
                    }
                    let _ = std::fs::remove_dir_all(&staging);
                    b.update.installing = false;
                    status(b, "error");
                    ui::update_downloads(b);
                    later(move || {
                        web::alert("Couldn't update Volant", &why, &["OK"]);
                    });
                }
            })
        });
    });
}

fn download_and_check(staging: &Path) -> Result<PathBuf, String> {
    let rel = release().map_err(|e| format!("Couldn't reach GitHub ({e})."))?;
    let want = rel.zip_sha256.ok_or("GitHub didn't say what the update should contain, so it wasn't installed.")?;
    let mut opts = super::net::options();
    opts.limit = 256 << 20;
    opts.deadline = std::time::Duration::from_secs(600);
    let r = super::net::get(&rel.zip_url, &opts).map_err(|e| format!("The download didn't finish ({e}). Check your connection and try again."))?;
    if !r.ok() {
        return Err(format!("The download didn't finish (GitHub answered {}).", r.status));
    }
    // Exactly the file GitHub has: a damaged or altered download stops here.
    if hash::hex(&hash::sha256(&r.body)) != want {
        return Err("The download didn't match the release, so it wasn't installed.".into());
    }
    zip::extract_to(&r.body, staging, 1 << 30).map_err(|e| format!("The update couldn't be unpacked ({e})."))?;
    let new_app = staging.join("Volant.app");
    let plist = std::fs::read_to_string(new_app.join("Contents/Info.plist")).unwrap_or_default();
    let exe = std::fs::read(new_app.join("Contents/MacOS/Volant")).unwrap_or_default();
    let mach_o = volant_core::macho::slices(&exe).is_ok() || volant_core::macho::cpu(&exe).is_ok();
    if !plist.contains(BUNDLE_ID) || !mach_o {
        return Err("The download wasn't a copy of Volant.".into());
    }
    Ok(new_app)
}

/// Starts this very binary as a small helper (`--finish-update`) that waits for
/// Volant to quit, swaps the bundles and opens the new one; then quits.
fn swap_and_relaunch(app: &Path, new_app: &Path, staging: &Path) {
    use std::os::unix::process::CommandExt;
    let Ok(me) = std::env::current_exe() else { return };
    let spawned = Command::new(me)
        // Its own name in the process list, so nothing mistakes it for Volant.
        .arg0("volant-update-helper")
        .arg("--finish-update")
        .arg(std::process::id().to_string())
        .arg(app)
        .arg(new_app)
        .arg(staging)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok();
    if spawned {
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).terminate(None);
        }
    }
}

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// `Volant --finish-update PID APP NEW STAGING`: the second half of an update,
/// run as a helper process. Waits for Volant (PID) to quit, moves the old app
/// aside, moves the new one into place (putting the old one back if that
/// fails), cleans up, and opens the app. Returns the exit status.
pub fn finish_update(args: &[String]) -> i32 {
    let [pid, app, new, staging] = args else { return 2 };
    let Ok(pid) = pid.parse::<i32>() else { return 2 };
    let (app, new, staging) = (PathBuf::from(app), PathBuf::from(new), PathBuf::from(staging));
    // Only ever swap an app bundle for one staged beside it.
    if app.extension().map(|e| e != "app").unwrap_or(true) || !new.starts_with(&staging) || staging.parent() != app.parent() {
        return 2;
    }
    let started = std::time::Instant::now();
    while unsafe { kill(pid, 0) } == 0 && started.elapsed() < std::time::Duration::from_secs(120) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let mut old = app.clone().into_os_string();
    old.push(".previous");
    let old = PathBuf::from(old);
    let _ = std::fs::remove_dir_all(&old);
    if std::fs::rename(&app, &old).is_ok() {
        if std::fs::rename(&new, &app).is_ok() {
            let _ = std::fs::remove_dir_all(&old);
        } else {
            let _ = std::fs::rename(&old, &app);
        }
    }
    let _ = std::fs::remove_dir_all(&staging);
    // Open it the way the Finder would, and wait (briefly) until macOS has.
    let (tx, rx) = std::sync::mpsc::channel();
    let done = block2::RcBlock::new(move |_app: *mut objc2_app_kit::NSRunningApplication, _err: *mut objc2_foundation::NSError| {
        let _ = tx.send(());
    });
    let url = NSURL::fileURLWithPath(&super::ns(&app.to_string_lossy()));
    let config = NSWorkspaceOpenConfiguration::configuration();
    config.setActivates(true);
    NSWorkspace::sharedWorkspace().openApplicationAtURL_configuration_completionHandler(&url, &config, Some(&done));
    let _ = rx.recv_timeout(std::time::Duration::from_secs(20));
    0
}
