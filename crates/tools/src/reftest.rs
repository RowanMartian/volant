//! web-platform-tests reftests, run on Volant's own engine: does a test page
//! draw the same pixels as its reference?
//!
//! `reftest WPT_DIR LIST OUT_DIR [--jobs N] [--shots] [--baseline FILE [--write-baseline | --tolerance N]]`
//! finds every reftest (a page with `<link rel=match>` or `rel=mismatch`)
//! that LIST names, renders it and its references 800 x 600 px, as WPT
//! does, through the whole engine (scripts, style, layout) and the software
//! rasterizer, and compares the pixels, allowing what a `<meta name=fuzzy>`
//! allows. Each file runs in a process of its own. It writes
//! `OUT_DIR/reftest.md` (pass rates by folder) and `OUT_DIR/reftest.tsv`;
//! `--shots` also saves both images of each failure under `OUT_DIR/shots/`.
//! With `--baseline`, it fails if a file that passed there fails now.
//!
//! Both sides go through the same rasterizer, whose text is a mark per
//! character rather than real glyphs, so what's compared is layout: where
//! boxes and characters land, their sizes and colours.

use crate::wpt::{WptHost, ORIGIN};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use volant_engine::raster::{self, Bitmap, Canvas};
use volant_engine::script::{self, Host};

const W: u32 = 800;
const H: u32 = 600;
/// How long one file (the test and its references) may take.
const LIMIT: Duration = Duration::from_secs(30);
/// How long a `reftest-wait` page may take to say it's ready.
const WAIT: Duration = Duration::from_secs(5);

/// A page's references: (`match` or `mismatch`, URL).
fn refs(html: &str, url: &str) -> Vec<(bool, String)> {
    let dom = volant_core::html::parse(html);
    let mut out = Vec::new();
    for n in 0..dom.nodes.len() as u32 {
        if dom.tag(n) != Some("link") {
            continue;
        }
        let rel = dom.attr(n, "rel").unwrap_or("").to_ascii_lowercase();
        let Some(href) = dom.attr(n, "href") else { continue };
        let is_match = rel.split_whitespace().any(|r| r == "match");
        if is_match || rel.split_whitespace().any(|r| r == "mismatch") {
            out.push((is_match, volant_core::url::resolve(url, href).unwrap_or_default()));
        }
    }
    out
}

/// `<meta name=fuzzy content="[ref:]maxDifference=a-b;totalPixels=c-d">`: the allowed (max difference, pixel count) ranges for a reference.
fn fuzzy(html: &str, url: &str, reference: &str) -> Option<((u32, u32), (u64, u64))> {
    let dom = volant_core::html::parse(html);
    let range = |v: &str| -> Option<(u64, u64)> {
        let v = v.trim();
        Some(match v.split_once('-') {
            Some((a, b)) => (a.trim().parse().ok()?, b.trim().parse().ok()?),
            None => {
                let n = v.parse().ok()?;
                (n, n)
            }
        })
    };
    for n in 0..dom.nodes.len() as u32 {
        if dom.tag(n) != Some("meta") || dom.attr(n, "name") != Some("fuzzy") {
            continue;
        }
        let content = dom.attr(n, "content").unwrap_or("");
        // An optional "reference-url:" names the reference it's for.
        let (target, spec) = match content.split_once(':') {
            Some((t, s)) if !t.contains('=') && !t.contains(';') => (Some(t.trim()), s),
            _ => (None, content),
        };
        if let Some(t) = target {
            if volant_core::url::resolve(url, t).as_deref() != Some(reference) {
                continue;
            }
        }
        let parts: Vec<&str> = spec.split(';').collect();
        let (mut diff, mut total) = (None, None);
        for (k, p) in parts.iter().enumerate() {
            match p.split_once('=') {
                Some((name, v)) if name.trim() == "maxDifference" => diff = range(v),
                Some((name, v)) if name.trim() == "totalPixels" => total = range(v),
                None if k == 0 => diff = range(p),
                None if k == 1 => total = range(p),
                _ => {}
            }
        }
        if let (Some(d), Some(t)) = (diff, total) {
            return Some(((d.0 as u32, d.1 as u32), t));
        }
    }
    None
}

struct Images(HashMap<String, Bitmap>);

impl volant_engine::Images for Images {
    fn size(&self, src: &str) -> Option<(f32, f32)> {
        self.0.get(src).map(|b| (b.width as f32, b.height as f32))
    }
}

/// Renders the page at `url` as WPT's reftests see it.
fn render(host: &WptHost, url: &str) -> Result<(Canvas, String), String> {
    if url == "about:blank" {
        return Ok((Canvas::new(W, H, volant_engine::Color { r: 255, g: 255, b: 255, a: 255 }), String::new()));
    }
    let headers = Vec::new();
    let req = script::Request { method: "GET", url, headers: &headers, body: None, credentials: true, initiator: None, navigation: true };
    let reply = host.request(&req).filter(|r| r.status == 200).ok_or_else(|| format!("{url} not found"))?;
    let (html, charset) = volant_core::encoding::decode_html(&reply.body, &reply.content_type);
    let host: std::rc::Rc<dyn script::Host> = std::rc::Rc::new(WptHost { root: host.root.clone() });
    let env = volant_engine::Env { width: W as f32, dark: false };
    let mut rt = script::Runtime::new_typed(&html, url, &reply.content_type, env, host.clone());
    rt.state.borrow_mut().charset = charset;
    rt.budget = 500_000_000;
    rt.run_scripts();
    // `class=reftest-wait` on the root: the page says when it's ready by taking it off.
    let waiting = "(function(){var e=document.documentElement;return !!(e&&e.classList&&e.classList.contains('reftest-wait'))})()";
    let started = Instant::now();
    loop {
        rt.tick();
        let wait = rt.interp.run(waiting).map(|v| volant_js::interp::Interp::truthy(&v)).unwrap_or(false);
        if !wait || started.elapsed() > WAIT {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let snap = rt.snapshot();
    let mut sheets = Vec::new();
    for (href, _media) in script::stylesheet_links(&snap.dom, &snap.url) {
        if let Some(r) = host.get_for(&href, &snap.url).filter(|r| r.success()) {
            sheets.push(volant_core::encoding::decode_html(&r.body, &r.content_type).0);
        }
    }
    // The page's pictures, by the src they're asked for with.
    let mut images = HashMap::new();
    for n in 0..snap.dom.nodes.len() as u32 {
        if snap.dom.tag(n) != Some("img") {
            continue;
        }
        let Some(src) = snap.dom.attr(n, "src") else { continue };
        if images.contains_key(src) {
            continue;
        }
        let abs = volant_core::url::resolve(&snap.url, src).unwrap_or_default();
        if let Some(r) = host.get_for(&abs, &snap.url).filter(|r| r.success()) {
            if let Ok((w, h, rgba)) = volant_core::png::decode(&r.body) {
                images.insert(src.to_string(), Bitmap { width: w, height: h, rgba });
            }
        }
    }
    let images = Images(images);
    let page = script::render_snapshot(&snap, &sheets, env, &volant_engine::ApproxFonts, &images);
    Ok((raster::draw(&page, W, H, &volant_engine::ApproxFonts, &images.0), html))
}

fn shot_name(path: &str) -> String {
    path.replace('/', "__")
}

/// One reftest, in this process: prints `R\tstatus\tmessage`.
pub fn one(root: &str, path: &str, shots: Option<&str>) {
    let host = WptHost { root: PathBuf::from(root) };
    let url = format!("{ORIGIN}/{}", path.trim_start_matches('/'));
    let (test, html) = match render(&host, &url) {
        Ok(x) => x,
        Err(e) => {
            println!("R\tERROR\t{e}");
            return;
        }
    };
    let list = refs(&html, &url);
    if list.is_empty() {
        println!("R\tERROR\tno reference");
        return;
    }
    for (is_match, r) in list {
        let reference = match render(&host, &r) {
            Ok((c, _)) => c,
            Err(e) => {
                println!("R\tERROR\treference: {e}");
                return;
            }
        };
        let (n, most) = raster::diff(&test, &reference);
        let allowed = match fuzzy(&html, &url, &r) {
            Some(((_, dmax), (_, tmax))) => n == 0 || (most as u32 <= dmax && n <= tmax),
            None => n == 0,
        };
        let short = r.strip_prefix(ORIGIN).unwrap_or(&r).to_string();
        if allowed != is_match {
            if let Some(dir) = shots {
                let base = format!("{dir}/{}", shot_name(path));
                let _ = std::fs::write(format!("{base}.test.png"), volant_core::png::encode(W, H, &test.rgba));
                let _ = std::fs::write(format!("{base}.ref.png"), volant_core::png::encode(W, H, &reference.rgba));
            }
            let why = if is_match { format!("{n} pixels differ (by up to {most}) from {short}") } else { format!("draws the same as {short}, which it mustn't") };
            println!("R\tFAIL\t{why}");
            return;
        }
    }
    println!("R\tPASS\t");
}

/// Is this file a reftest (not a reference or support file)?
fn is_reftest(root: &Path, rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if rel.split('/').any(|d| matches!(d, "support" | "resources" | "tools" | "reference" | "references")) || name.starts_with('.') {
        return false;
    }
    if name.contains("-ref.") || name.contains("-notref.") || name.contains("-manual.") || name.contains("-crash.") || name.contains("-print.") {
        return false;
    }
    if !(name.ends_with(".html") || name.ends_with(".htm") || name.ends_with(".xht") || name.ends_with(".xhtml")) {
        return false;
    }
    std::fs::read_to_string(root.join(rel)).map(|t| (t.contains("match") && t.contains("<link")) && !t.contains("testharness.js")).unwrap_or(false)
        && std::fs::read_to_string(root.join(rel)).map(|t| !refs(&t, &format!("{ORIGIN}/{rel}")).is_empty()).unwrap_or(false)
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
                if !matches!(n.as_str(), "support" | "resources" | "tools" | "reference" | "references") {
                    expand(root, &rel, out);
                }
            } else if is_reftest(root, &rel) {
                out.push(rel);
            }
        }
    } else {
        out.push(entry.to_string());
    }
}

fn child(root: &str, path: &str, shots: Option<&str>) -> (String, String) {
    let Ok(exe) = std::env::current_exe() else { return ("CRASH".into(), String::new()) };
    let mut args = vec!["reftest-one".to_string(), root.to_string(), path.to_string()];
    if let Some(s) = shots {
        args.push(s.to_string());
    }
    let Ok(mut c) = Command::new(exe).args(&args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else { return ("CRASH".into(), String::new()) };
    let started = Instant::now();
    loop {
        match c.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > LIMIT => {
                let _ = c.kill();
                let _ = c.wait();
                return ("TIMEOUT".into(), String::new());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return ("CRASH".into(), String::new()),
        }
    }
    let mut out = String::new();
    if let Some(mut o) = c.stdout.take() {
        use std::io::Read;
        let _ = o.read_to_string(&mut out);
    }
    for line in out.lines() {
        let f: Vec<&str> = line.splitn(3, '\t').collect();
        if f.first() == Some(&"R") {
            return (f.get(1).unwrap_or(&"").to_string(), f.get(2).unwrap_or(&"").to_string());
        }
    }
    ("CRASH".into(), String::new())
}

pub fn run(args: &[String]) -> Result<(), String> {
    let root = args.first().ok_or("reftest needs a web-platform-tests checkout")?.clone();
    let list = args.get(1).ok_or("reftest needs a list of tests")?;
    let dir = args.get(2).ok_or("reftest needs an output folder")?.clone();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let jobs: usize = flag("--jobs").and_then(|j| j.parse().ok()).unwrap_or(4);
    let shots = args.iter().any(|a| a == "--shots").then(|| format!("{dir}/shots"));
    let text = std::fs::read_to_string(list).map_err(|e| format!("{list}: {e}"))?;
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    let skip: Vec<&str> = lines.iter().filter_map(|l| l.strip_prefix('!')).map(|l| l.trim().trim_matches('/')).collect();
    let mut tests = Vec::new();
    for l in lines.iter().filter(|l| !l.starts_with('!')) {
        expand(Path::new(&root), l, &mut tests);
    }
    tests.retain(|t| !skip.iter().any(|s| t == s || t.starts_with(&format!("{s}/"))));
    tests.dedup();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if let Some(s) = &shots {
        std::fs::create_dir_all(s).map_err(|e| e.to_string())?;
    }
    let started = Instant::now();
    let queue = std::sync::Arc::new(std::sync::Mutex::new(tests.iter().cloned().enumerate().rev().collect::<Vec<_>>()));
    let (tx, rx) = std::sync::mpsc::channel();
    for _ in 0..jobs.max(1) {
        let (queue, tx, root, shots) = (queue.clone(), tx.clone(), root.clone(), shots.clone());
        std::thread::spawn(move || loop {
            let next = queue.lock().unwrap().pop();
            let Some((i, path)) = next else { break };
            let r = child(&root, &path, shots.as_deref());
            let _ = tx.send((i, path, r));
        });
    }
    drop(tx);
    let mut results: Vec<(usize, String, (String, String))> = Vec::new();
    for (i, path, r) in rx {
        eprintln!("[{}/{}] {path} — {} {}", results.len() + 1, tests.len(), r.0, r.1);
        results.push((i, path, r));
    }
    results.sort_by_key(|(i, ..)| *i);
    let tsv: String = results.iter().map(|(_, p, (st, m))| format!("{p}\t{st}\t{m}\n")).collect();
    std::fs::write(format!("{dir}/reftest.tsv"), tsv).map_err(|e| e.to_string())?;
    let md = report(&results, started.elapsed());
    std::fs::write(format!("{dir}/reftest.md"), &md).map_err(|e| e.to_string())?;
    println!("{}", md.split("\n## Every failure").next().unwrap_or(&md));
    let counts: String = results.iter().map(|(_, p, (st, _))| format!("{p}\t{}\n", (st == "PASS") as u8)).collect();
    if args.iter().any(|a| a == "--write-baseline") {
        let file = flag("--baseline").ok_or("--write-baseline needs --baseline FILE")?;
        std::fs::write(&file, counts).map_err(|e| format!("{file}: {e}"))?;
        println!("wrote {file}");
    } else if let Some(file) = flag("--baseline") {
        let base = std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?;
        let now: BTreeMap<&str, bool> = results.iter().map(|(_, p, (st, _))| (p.as_str(), st == "PASS")).collect();
        let (mut worse, mut better) = (Vec::new(), 0);
        for l in base.lines() {
            let Some((p, n)) = l.split_once('\t') else { continue };
            match (n.trim() == "1", now.get(p)) {
                (true, Some(false)) => worse.push(p),
                (false, Some(true)) => better += 1,
                _ => {}
            }
        }
        if better > 0 {
            println!("{better} more files pass than the baseline; update it with --write-baseline.");
        }
        let tolerance: usize = flag("--tolerance").and_then(|t| t.parse().ok()).unwrap_or(0);
        if worse.len() > tolerance {
            return Err(format!("{} files that passed fail now:\n{}", worse.len(), worse.join("\n")));
        }
        if !worse.is_empty() {
            println!("Within the tolerance, these fail now:\n{}", worse.join("\n"));
        }
    }
    Ok(())
}

fn report(results: &[(usize, String, (String, String))], took: Duration) -> String {
    let pct = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f64 * 100.0 / b as f64 };
    let pass = results.iter().filter(|r| r.2 .0 == "PASS").count();
    let mut md = String::from("# web-platform-tests reftests on Volant's engine\n\n");
    md.push_str(&format!("{pass} of {} reftests pass ({:.1}%), in {:.0} s.\n\n", results.len(), pct(pass, results.len()), took.as_secs_f64()));
    md.push_str("## By folder\n\n| Folder | Passing | Errors or timeouts |\n|---|---|---|\n");
    let mut by: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    for (_, p, (st, _)) in results {
        // Two levels down (css/css-flexbox), or three for CSS2's chapters.
        let parts: Vec<&str> = p.split('/').collect();
        let depth = if parts.get(1) == Some(&"CSS2") { 3 } else { 2 };
        let folder = parts[..parts.len().saturating_sub(1).min(depth)].join("/");
        let e = by.entry(folder).or_default();
        e.0 += (st == "PASS") as usize;
        e.1 += 1;
        e.2 += matches!(st.as_str(), "ERROR" | "TIMEOUT" | "CRASH") as usize;
    }
    for (folder, (p, n, bad)) in &by {
        md.push_str(&format!("| {folder} | {p} / {n} ({:.0}%) | {bad} |\n", pct(*p, *n)));
    }
    md.push_str("\n## Every failure\n\n| File | Result |\n|---|---|\n");
    for (_, p, (st, m)) in results.iter().filter(|r| r.2 .0 != "PASS") {
        md.push_str(&format!("| {p} | {st} {} |\n", m.replace('|', "\\|")));
    }
    md
}
