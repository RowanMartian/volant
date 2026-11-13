//! A slice of web-platform-tests, run on Volant's own engine.
//!
//! `wpt WPT_DIR LIST OUT_DIR [--jobs N] [--baseline FILE] [--write-baseline]`
//! runs every testharness.js test that LIST names (files or folders, relative
//! to a checkout of github.com/web-platform-tests/wpt), each in a process of
//! its own, and writes `OUT_DIR/wpt.md` (pass rates by folder, the commonest
//! failures) and `OUT_DIR/wpt.tsv` (every subtest). With `--baseline`, it
//! fails if any file passes fewer subtests than the baseline says it did.
//!
//! Tests are served from `http://web-platform.test/`, out of the checkout;
//! `.any.js` and `.window.js` tests get the page wptserve would wrap them in.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use volant_engine::script;

const ORIGIN: &str = "http://web-platform.test";
/// How long one test file may take, everything included.
const LIMIT: Duration = Duration::from_secs(40);
/// testharness.js's own timeout for a normal test is 10 s, a long one 60 s; ours waits this long.
const WAIT: Duration = Duration::from_secs(12);

/// Takes testharness.js's results as they're reported, instead of drawing them.
const REPORT: &str = r#"
setup({ output: false, explicit_timeout: false });
add_completion_callback(function (tests, status) {
  var t = [];
  for (var i = 0; i < tests.length; i++) t.push([String(tests[i].name), tests[i].status, String(tests[i].message || "")]);
  self.__wpt_result = JSON.stringify({ status: status.status, message: String(status.message || ""), tests: t });
});
"#;

const TEST_STATUS: [&str; 5] = ["PASS", "FAIL", "TIMEOUT", "NOTRUN", "PRECONDITION_FAILED"];
const HARNESS_STATUS: [&str; 4] = ["OK", "ERROR", "TIMEOUT", "PRECONDITION_FAILED"];

struct WptHost {
    root: PathBuf,
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" | "htm" => "text/html",
        "xhtml" => "application/xhtml+xml",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        "xml" => "application/xml",
        "png" => "image/png",
        "txt" => "text/plain",
        _ => "",
    }
}

/// The page wptserve makes for `x.any.html` / `x.window.html` out of `x.any.js` / `x.window.js`.
fn wrapper(root: &Path, path: &str) -> Option<String> {
    let js = path.strip_suffix(".html")?.to_string() + ".js";
    if !(js.ends_with(".any.js") || js.ends_with(".window.js")) {
        return None;
    }
    let src = std::fs::read_to_string(root.join(js.trim_start_matches('/'))).ok()?;
    let mut page = String::from("<!doctype html>\n<meta charset=utf-8>\n");
    page.push_str("<script>self.GLOBAL = { isWindow: function () { return true; }, isWorker: function () { return false; }, isShadowRealm: function () { return false; } };</script>\n");
    page.push_str("<script src=\"/resources/testharness.js\"></script>\n<script src=\"/resources/testharnessreport.js\"></script>\n");
    let dir = &js[..js.rfind('/').unwrap_or(0) + 1];
    for line in src.lines() {
        let Some(meta) = line.strip_prefix("// META:") else { continue };
        if let Some(s) = meta.trim().strip_prefix("script=") {
            let s = s.trim();
            let url = if s.starts_with('/') || s.contains("://") { s.to_string() } else { format!("{dir}{s}") };
            page.push_str(&format!("<script src=\"{url}\"></script>\n"));
        } else if let Some(t) = meta.trim().strip_prefix("title=") {
            page.push_str(&format!("<title>{}</title>\n", t.trim()));
        }
    }
    page.push_str(&format!("<div id=log></div>\n<script src=\"{js}\"></script>\n"));
    Some(page)
}

impl script::Host for WptHost {
    fn request(&self, req: &script::Request) -> Option<script::Reply> {
        let rest = req.url.strip_prefix(ORIGIN)?;
        let path = rest.split(['?', '#']).next().unwrap_or(rest);
        let path = if path.is_empty() { "/" } else { path };
        if path == "/resources/testharnessreport.js" {
            return Some(script::Reply::ok(200, "text/javascript", REPORT.as_bytes().to_vec()));
        }
        let file = self.root.join(path.trim_start_matches('/'));
        match std::fs::read(&file) {
            Ok(b) => Some(script::Reply::ok(200, content_type(path), b)),
            Err(_) => match wrapper(&self.root, path) {
                Some(page) => Some(script::Reply::ok(200, "text/html", page.into_bytes())),
                None => Some(script::Reply::ok(404, "text/plain", b"not found".to_vec())),
            },
        }
    }
}

/// One test file, in this process: prints `H\tstatus\tmessage`, then a `T\tstatus\tname\tmessage` per subtest.
pub fn one(root: &str, path: &str) {
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let host = std::rc::Rc::new(WptHost { root: PathBuf::from(root) });
    let url = format!("{ORIGIN}/{}", path.trim_start_matches('/'));
    let headers = Vec::new();
    let req = script::Request { method: "GET", url: &url, headers: &headers, body: None, credentials: true, initiator: None, navigation: true };
    let reply = script::Host::request(&*host, &req).filter(|r| r.status == 200);
    let Some(reply) = reply else {
        println!("H\tMISSING\t{path} not found");
        return;
    };
    let html = String::from_utf8_lossy(&reply.body).into_owned();
    let host: std::rc::Rc<dyn script::Host> = host;
    let mut rt = script::Runtime::new(&html, &url, volant_engine::Env { width: 800.0, dark: false }, host);
    rt.run_scripts();
    let started = Instant::now();
    let probe = "typeof __wpt_result == 'string' ? __wpt_result : ''";
    let mut result = String::new();
    loop {
        rt.interp.reset_steps(50_000_000);
        if let Ok(v) = rt.interp.run(probe) {
            result = rt.interp.to_str(&v).map(|s| s.to_string()).unwrap_or_default();
        }
        if !result.is_empty() || started.elapsed() > WAIT {
            break;
        }
        rt.tick();
        std::thread::sleep(Duration::from_millis(if rt.has_timers() { 20 } else { 100 }));
    }
    let errors: Vec<String> = rt.take_console().into_iter().filter_map(|l| l.strip_prefix("Uncaught ").map(String::from)).collect();
    if result.is_empty() {
        let why = errors.first().map(|e| clean(e)).unwrap_or_else(|| "the harness never finished".into());
        println!("H\tTIMEOUT\t{}", why.chars().take(200).collect::<String>());
        return;
    }
    let Some(v) = volant_core::json::parse(&result) else {
        println!("H\tERROR\tunreadable results");
        return;
    };
    let status = v.get("status").and_then(|s| s.as_f64()).unwrap_or(1.0) as usize;
    let message = v.get("message").and_then(|s| s.as_str()).unwrap_or("");
    println!("H\t{}\t{}", HARNESS_STATUS.get(status).unwrap_or(&"ERROR"), clean(message).chars().take(200).collect::<String>());
    for t in v.get("tests").map(|t| t.as_array()).unwrap_or(&[]) {
        let t = t.as_array();
        let name = t.first().and_then(|x| x.as_str()).unwrap_or("");
        let st = t.get(1).and_then(|x| x.as_f64()).unwrap_or(1.0) as usize;
        let msg = t.get(2).and_then(|x| x.as_str()).unwrap_or("");
        println!("T\t{}\t{}\t{}", TEST_STATUS.get(st).unwrap_or(&"FAIL"), clean(name), clean(msg).chars().take(200).collect::<String>());
    }
}

/// What one test file came to.
struct FileResult {
    path: String,
    harness: String,
    message: String,
    /// (status, name, message)
    tests: Vec<(String, String, String)>,
}

impl FileResult {
    fn passed(&self) -> usize {
        self.tests.iter().filter(|t| t.0 == "PASS").count()
    }
}

fn child(root: &str, path: &str) -> FileResult {
    let mut r = FileResult { path: path.to_string(), harness: String::new(), message: String::new(), tests: Vec::new() };
    let Ok(exe) = std::env::current_exe() else {
        r.harness = "CRASH".into();
        return r;
    };
    let Ok(mut c) = Command::new(exe).args(["wpt-one", root, path]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
        r.harness = "CRASH".into();
        return r;
    };
    // Read as it comes, so a test with thousands of subtests can't fill the pipe and stall.
    let mut stdout = c.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        if let Some(o) = stdout.as_mut() {
            let _ = o.read_to_string(&mut out);
        }
        out
    });
    let started = Instant::now();
    let exit = loop {
        match c.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if started.elapsed() > LIMIT => {
                let _ = c.kill();
                let _ = c.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break None,
        }
    };
    let out = reader.join().unwrap_or_default();
    for line in out.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        match f.first() {
            Some(&"H") => {
                r.harness = f.get(1).unwrap_or(&"").to_string();
                r.message = f.get(2).unwrap_or(&"").to_string();
            }
            Some(&"T") => r.tests.push((f.get(1).unwrap_or(&"").to_string(), f.get(2).unwrap_or(&"").to_string(), f.get(3).unwrap_or(&"").to_string())),
            _ => {}
        }
    }
    if r.harness.is_empty() {
        r.harness = match exit {
            None => "TIMEOUT".into(),
            Some(s) => format!("CRASH ({s})"),
        };
    }
    r
}

/// Is this file a testharness.js test (not a reference, manual or crash test)?
fn is_test(root: &Path, rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if rel.split('/').any(|d| matches!(d, "support" | "resources" | "tools")) || name.starts_with('.') {
        return false;
    }
    if name.contains("-manual.") || name.contains("-ref.") || name.contains("-crash.") || name.contains("-notref.") {
        return false;
    }
    if name.ends_with(".any.js") || name.ends_with(".window.js") {
        return true;
    }
    if !(name.ends_with(".html") || name.ends_with(".htm")) {
        return false;
    }
    std::fs::read_to_string(root.join(rel)).map(|t| t.contains("testharness.js")).unwrap_or(false)
}

/// The URL path a test runs at (`x.any.js` runs as `x.any.html`).
fn test_url(rel: &str) -> String {
    match rel.strip_suffix(".js") {
        Some(base) if base.ends_with(".any") || base.ends_with(".window") => format!("{base}.html"),
        _ => rel.to_string(),
    }
}

fn expand(root: &Path, entry: &str, out: &mut Vec<String>) {
    let entry = entry.trim_matches('/');
    let full = root.join(entry);
    if full.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(&full).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        for n in names {
            let rel = format!("{entry}/{n}");
            if root.join(&rel).is_dir() {
                if !matches!(n.as_str(), "support" | "resources" | "tools") {
                    expand(root, &rel, out);
                }
            } else if is_test(root, &rel) {
                out.push(test_url(&rel));
            }
        }
    } else {
        out.push(test_url(entry));
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let root = args.first().ok_or("wpt needs a web-platform-tests checkout")?.clone();
    let list = args.get(1).ok_or("wpt needs a list of tests")?;
    let dir = args.get(2).ok_or("wpt needs an output folder")?;
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let jobs: usize = flag("--jobs").and_then(|j| j.parse().ok()).unwrap_or(4);
    let text = std::fs::read_to_string(list).map_err(|e| format!("{list}: {e}"))?;
    // Lines name folders or files to run; `!path` leaves a folder or file out.
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    let skip: Vec<&str> = lines.iter().filter_map(|l| l.strip_prefix('!')).map(|l| l.trim().trim_matches('/')).collect();
    let mut tests = Vec::new();
    for l in lines.iter().filter(|l| !l.starts_with('!')) {
        expand(Path::new(&root), l, &mut tests);
    }
    tests.retain(|t| !skip.iter().any(|s| t == s || t.starts_with(&format!("{s}/"))));
    tests.dedup();
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let queue = std::sync::Arc::new(std::sync::Mutex::new(tests.iter().cloned().enumerate().rev().collect::<Vec<_>>()));
    let (tx, rx) = std::sync::mpsc::channel();
    for _ in 0..jobs.max(1) {
        let (queue, tx, root) = (queue.clone(), tx.clone(), root.clone());
        std::thread::spawn(move || loop {
            let next = queue.lock().unwrap().pop();
            let Some((i, path)) = next else { break };
            let _ = tx.send((i, child(&root, &path)));
        });
    }
    drop(tx);
    let mut results: Vec<(usize, FileResult)> = Vec::new();
    for (i, r) in rx {
        eprintln!("[{}/{}] {} — {} ({}/{})", results.len() + 1, tests.len(), r.path, r.harness, r.passed(), r.tests.len());
        results.push((i, r));
    }
    results.sort_by_key(|(i, _)| *i);
    let results: Vec<FileResult> = results.into_iter().map(|(_, r)| r).collect();
    let mut tsv = String::new();
    for r in &results {
        tsv.push_str(&format!("{}\t\t{}\t{}\n", r.path, r.harness, r.message));
        for (st, name, msg) in &r.tests {
            tsv.push_str(&format!("{}\t{}\t{}\t{}\n", r.path, name, st, msg));
        }
    }
    std::fs::write(format!("{dir}/wpt.tsv"), tsv).map_err(|e| e.to_string())?;
    let md = report(&results, started.elapsed());
    std::fs::write(format!("{dir}/wpt.md"), &md).map_err(|e| e.to_string())?;
    println!("{}", md.split("\n## Every file").next().unwrap_or(&md));
    let counts: String = results.iter().map(|r| format!("{}\t{}\n", r.path, r.passed())).collect();
    if args.iter().any(|a| a == "--write-baseline") {
        let file = flag("--baseline").ok_or("--write-baseline needs --baseline FILE")?;
        std::fs::write(&file, counts).map_err(|e| format!("{file}: {e}"))?;
        println!("wrote {file}");
    } else if let Some(file) = flag("--baseline") {
        let base = std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?;
        let now: BTreeMap<&str, usize> = results.iter().map(|r| (r.path.as_str(), r.passed())).collect();
        let mut worse = Vec::new();
        let mut better = 0;
        for l in base.lines() {
            let Some((p, n)) = l.split_once('\t') else { continue };
            let n: usize = n.trim().parse().unwrap_or(0);
            match now.get(p) {
                Some(&m) if m < n => worse.push(format!("{p}: {m} passing, was {n}")),
                Some(&m) if m > n => better += m - n,
                _ => {}
            }
        }
        if better > 0 {
            println!("{better} more subtests pass than the baseline; update it with --write-baseline.");
        }
        if !worse.is_empty() {
            return Err(format!("fewer subtests pass than the baseline:\n{}", worse.join("\n")));
        }
    }
    Ok(())
}

fn report(results: &[FileResult], took: Duration) -> String {
    let pct = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f64 * 100.0 / b as f64 };
    let (pass, total): (usize, usize) = results.iter().fold((0, 0), |(p, t), r| (p + r.passed(), t + r.tests.len()));
    let clean = results.iter().filter(|r| r.harness == "OK" && !r.tests.is_empty() && r.passed() == r.tests.len()).count();
    let mut md = String::from("# web-platform-tests on Volant's engine\n\n");
    md.push_str(&format!(
        "{pass} of {total} subtests pass ({:.1}%), in {} test files ({clean} pass completely), in {:.0} s.\n\n",
        pct(pass, total),
        results.len(),
        took.as_secs_f64()
    ));
    md.push_str("## By folder\n\n| Folder | Subtests passing | Files fully passing | Harness errors or timeouts |\n|---|---|---|---|\n");
    let mut by: BTreeMap<String, (usize, usize, usize, usize, usize)> = BTreeMap::new();
    for r in results {
        let folder = r.path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        let e = by.entry(folder).or_default();
        e.0 += r.passed();
        e.1 += r.tests.len();
        e.2 += (r.harness == "OK" && !r.tests.is_empty() && r.passed() == r.tests.len()) as usize;
        e.3 += 1;
        e.4 += (r.harness != "OK") as usize;
    }
    for (folder, (p, t, c, n, bad)) in &by {
        md.push_str(&format!("| {folder} | {p} / {t} ({:.0}%) | {c} / {n} | {bad} |\n", pct(*p, *t)));
    }
    // The commonest failure messages point at the missing pieces.
    let mut msgs: BTreeMap<String, usize> = BTreeMap::new();
    for r in results {
        if r.harness != "OK" {
            *msgs.entry(format!("harness {}: {}", r.harness, r.message.chars().take(100).collect::<String>())).or_default() += 1;
        }
        for (st, _, m) in &r.tests {
            if st != "PASS" {
                let m: String = m.chars().take(100).collect();
                *msgs.entry(format!("{st}: {m}")).or_default() += 1;
            }
        }
    }
    let mut msgs: Vec<(String, usize)> = msgs.into_iter().collect();
    msgs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    md.push_str("\n## Commonest failures\n\n| Count | Failure |\n|---|---|\n");
    for (m, n) in msgs.iter().take(40) {
        md.push_str(&format!("| {n} | {} |\n", m.replace('|', "\\|")));
    }
    md.push_str("\n## Every file\n\n| File | Harness | Passing |\n|---|---|---|\n");
    for r in results {
        md.push_str(&format!("| {} | {} | {} / {} |\n", r.path, r.harness, r.passed(), r.tests.len()));
    }
    md
}
