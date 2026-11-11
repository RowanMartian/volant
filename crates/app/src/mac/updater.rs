//! Updates in place.
//!
//! Every GitHub build of Volant knows the commit it was built from. The updater
//! asks GitHub which commit the "Latest build" release was made from; if it's a
//! different one, an update is offered. Installing downloads the zip, unpacks
//! it next to the running app, checks it really is Volant with an intact code
//! signature, then swaps the two app bundles the moment Volant quits and opens
//! the new one. Your tabs, history and settings live outside the app and stay.

use super::{later, on_main, pages, ui, web, with, Browser, COMMIT};
use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSBundle, NSData, NSString, NSURL};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use volant_core::json;

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
    let api = format!("https://api.github.com/repos/{REPO}/releases/tags/latest");
    std::thread::spawn(move || {
        let text = NSURL::URLWithString(&NSString::from_str(&api))
            .and_then(|u| NSData::dataWithContentsOfURL(&u))
            .and_then(|d| String::from_utf8(d.to_vec()).ok());
        on_main(move || {
            with(move |b| {
                b.update.checking = false;
                b.update.last_check = super::now();
                let latest = text.as_deref().and_then(|t| json::strings_for(t, "target_commitish").into_iter().next());
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
    let zip_url = format!("https://github.com/{REPO}/releases/download/latest/Volant-mac.zip");
    std::thread::spawn(move || {
        let result = download_and_check(&zip_url, &staging);
        on_main(move || {
            with(move |b| match result {
                Ok(new_app) => {
                    // Save the tabs now; quitting happens after this borrow ends.
                    b.session_dirty = true;
                    super::actions::save_session(b);
                    later(move || swap_and_relaunch(&app, &new_app, &staging));
                }
                Err(why) => {
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

fn download_and_check(url: &str, staging: &Path) -> Result<PathBuf, String> {
    let data = NSURL::URLWithString(&NSString::from_str(url))
        .and_then(|u| NSData::dataWithContentsOfURL(&u))
        .ok_or("The download didn't finish. Check your connection and try again.")?;
    let zip = staging.join("Volant-mac.zip");
    std::fs::write(&zip, data.to_vec()).map_err(|e| format!("Couldn't save the update: {e}"))?;
    let ok = Command::new("/usr/bin/ditto").args(["-x", "-k"]).arg(&zip).arg(staging).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        return Err("The update couldn't be unpacked.".into());
    }
    let new_app = staging.join("Volant.app");
    let plist = std::fs::read_to_string(new_app.join("Contents/Info.plist")).unwrap_or_default();
    if !plist.contains(BUNDLE_ID) || !new_app.join("Contents/MacOS/Volant").exists() {
        return Err("The download wasn't a copy of Volant.".into());
    }
    // The signature seals every file in the bundle; a damaged download fails here.
    let sealed = Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(&new_app).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
    if !sealed {
        return Err("The update's signature didn't check out, so it wasn't installed.".into());
    }
    Ok(new_app)
}

/// A tiny script waits for Volant to quit, swaps the bundles (rolling back if
/// the swap fails), cleans up, and opens the new Volant.
fn swap_and_relaunch(app: &Path, new_app: &Path, staging: &Path) {
    const SCRIPT: &str = r#"
pid="$1"; app="$2"; new="$3"; dir="$4"
while kill -0 "$pid" 2>/dev/null; do sleep 0.1; done
old="$app.previous"
rm -rf "$old"
if mv "$app" "$old"; then
  if mv "$new" "$app"; then rm -rf "$old"; else mv "$old" "$app"; fi
fi
rm -rf "$dir"
open "$app"
"#;
    let spawned = Command::new("/bin/sh")
        .args(["-c", SCRIPT, "volant-update"])
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
