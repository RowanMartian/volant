//! The compatibility survey: real sites through the same steps as Canard
//! (fetch, parse, scripts, a moment of timers, the fallback check, style
//! sheets, layout), each in a process of its own so a crash or a hang is one
//! row of the report, not the end of it.
//!
//! `survey SITES.txt OUT_DIR` writes `OUT_DIR/survey.md` (summary, top errors,
//! slowest phases, every site) and `OUT_DIR/survey.tsv` (one line per site, to
//! compare runs).

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use volant_engine::script;

/// How long one site may take, everything included.
const LIMIT: Duration = Duration::from_secs(90);

/// What one site came to.
#[derive(Default, Clone)]
struct Row {
    url: String,
    /// "shown", or why Canard wouldn't show it.
    verdict: String,
    status: u16,
    kb: usize,
    scripts: usize,
    errors: usize,
    /// Distinct uncaught errors, in order.
    messages: Vec<String>,
    text: usize,
    items: usize,
    /// Milliseconds: fetch, parse, scripts (running), scripts (downloading), timers, style sheets, layout.
    ms: [f64; 7],
}

const PHASES: [&str; 7] = ["fetch", "parse", "scripts: JavaScript", "scripts: downloads", "timers", "css", "layout"];

/// The network, timed: how much of a phase was spent waiting for it.
struct TimedHost {
    spent: std::cell::Cell<f64>,
}

impl script::Host for TimedHost {
    fn request(&self, req: &script::Request) -> Option<script::Reply> {
        let t = Instant::now();
        let r = crate::CurlHost.request(req);
        self.spent.set(self.spent.get() + t.elapsed().as_secs_f64() * 1000.0);
        r
    }
    fn background(&self) -> Option<script::Fetch> {
        script::Host::background(&crate::CurlHost)
    }
}

fn clean(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

impl Row {
    fn to_line(&self) -> String {
        let ms: Vec<String> = self.ms.iter().map(|m| format!("{m:.0}")).collect();
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.url,
            clean(&self.verdict),
            self.status,
            self.kb,
            self.scripts,
            self.errors,
            self.text,
            self.items,
            ms.join(","),
            self.messages.iter().map(|m| clean(m)).collect::<Vec<_>>().join(" || ")
        )
    }

    fn from_line(l: &str) -> Option<Row> {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 10 {
            return None;
        }
        let mut ms = [0.0; 7];
        for (i, m) in f[8].split(',').enumerate().take(7) {
            ms[i] = m.parse().unwrap_or(0.0);
        }
        Some(Row {
            url: f[0].into(),
            verdict: f[1].into(),
            status: f[2].parse().ok()?,
            kb: f[3].parse().ok()?,
            scripts: f[4].parse().ok()?,
            errors: f[5].parse().ok()?,
            text: f[6].parse().ok()?,
            items: f[7].parse().ok()?,
            ms,
            messages: f[9].split(" || ").filter(|m| !m.is_empty()).map(String::from).collect(),
        })
    }
}

/// One site, in this process: prints its row.
pub fn one(url: &str) {
    let mut row = Row { url: url.to_string(), ..Default::default() };
    let timed = std::rc::Rc::new(TimedHost { spent: std::cell::Cell::new(0.0) });
    let host: std::rc::Rc<dyn script::Host> = timed.clone();
    let t = Instant::now();
    let headers = vec![("Accept".to_string(), "text/html,application/xhtml+xml,*/*;q=0.8".to_string())];
    let req = script::Request { method: "GET", url, headers: &headers, body: None, credentials: true, initiator: None, navigation: true };
    let reply = host.request(&req);
    row.ms[0] = t.elapsed().as_secs_f64() * 1000.0;
    let Some(reply) = reply else {
        row.verdict = "fetch failed".into();
        println!("{}", row.to_line());
        return;
    };
    row.status = reply.status;
    row.kb = reply.body.len() / 1024;
    let page = if reply.url.is_empty() { url.to_string() } else { reply.url.clone() };
    let ty = reply.content_type.to_ascii_lowercase();
    if !(ty.is_empty() || ty.contains("html") || ty.contains("xml")) {
        row.verdict = format!("not a web page ({ty})");
    } else if volant_core::sites::is_app_url(&page) {
        row.verdict = "redirects to a web app".into();
    } else if matches!(reply.status, 401 | 402 | 403 | 429) {
        // Bot protection turning away a script from a data centre; says nothing about the engine.
        row.verdict = format!("refused the survey (HTTP {})", reply.status);
    } else if reply.status >= 400 {
        row.verdict = format!("HTTP {}", reply.status);
    }
    if !row.verdict.is_empty() {
        println!("{}", row.to_line());
        return;
    }
    let (html, charset) = volant_core::encoding::decode_html(&reply.body, &reply.content_type);
    let env = volant_engine::Env { width: 1100.0, height: 800.0, dark: false };
    let t = Instant::now();
    let mut rt = script::Runtime::new(&html, &page, env, host.clone());
    rt.state.borrow_mut().charset = charset;
    // VOLANT_PROFILE=1: where the page's JavaScript spends its time (to stderr).
    let profile = std::env::var_os("VOLANT_PROFILE").is_some();
    if profile {
        rt.interp.profile = Some(Default::default());
    }
    row.ms[1] = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    timed.spent.set(0.0);
    let waited = rt.preload_waited();
    rt.run_scripts();
    row.ms[3] = timed.spent.get() + rt.preload_waited() - waited;
    row.ms[2] = (t.elapsed().as_secs_f64() * 1000.0 - row.ms[3]).max(0.0);
    // Canard judges the page 1.5 s after its scripts; timers run meanwhile.
    let t = Instant::now();
    let mut console = rt.take_console();
    while t.elapsed() < Duration::from_millis(1500) {
        rt.tick();
        console.extend(rt.take_console());
        std::thread::sleep(Duration::from_millis(if rt.has_timers() { 150 } else { 500 }));
    }
    let busy = t.elapsed().as_secs_f64() * 1000.0 - 1500.0;
    row.ms[4] = busy.max(0.0);
    for line in console {
        if let Some(e) = line.strip_prefix("Uncaught ") {
            row.errors += 1;
            let e: String = e.chars().take(160).collect();
            if !row.messages.contains(&e) && row.messages.len() < 6 {
                row.messages.push(e);
            }
        }
    }
    let snap = rt.snapshot();
    row.verdict = script::assess(&snap, row.errors).unwrap_or("shown").to_string();
    let (scripts, text) = count(&snap.dom);
    row.scripts = scripts;
    row.text = text;
    let t = Instant::now();
    let mut sheets = Vec::new();
    for (href, media) in script::stylesheet_links(&snap.dom, &snap.url) {
        if let Some(r) = rt.host().get_for(&href, &snap.url).filter(|r| r.success()) {
            let text = String::from_utf8_lossy(&r.body).into_owned();
            sheets.push(if media.is_empty() || media == "all" { text } else { format!("@media {media} {{\n{text}\n}}") });
        }
    }
    row.ms[5] = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    if profile {
        eprintln!("JavaScript profile (share of samples, function, where it starts):\n{}", rt.interp.profile_report(30));
    }
    let laid = script::render_snapshot(&snap, &sheets, env, &volant_engine::ApproxFonts, &crate::PreviewImages);
    row.ms[6] = t.elapsed().as_secs_f64() * 1000.0;
    row.items = laid.items.len();
    if row.verdict == "shown" && laid.items.len() < 5 {
        row.verdict = "laid out empty".into();
    }
    println!("{}", row.to_line());
}

/// Scripts in the page, and visible characters of text.
fn count(dom: &volant_core::html::Dom) -> (usize, usize) {
    use volant_core::html::Data;
    let (mut scripts, mut text) = (0, 0);
    let mut stack = vec![volant_core::html::ROOT];
    while let Some(id) = stack.pop() {
        match dom.tag(id) {
            Some("script") => {
                scripts += 1;
                continue;
            }
            Some("style" | "noscript" | "template" | "head") => continue,
            _ => {}
        }
        if let Data::Text(t) = &dom.nodes[id as usize].data {
            text += t.chars().filter(|c| !c.is_whitespace()).count();
        }
        stack.extend(dom.children(id).iter().copied());
    }
    (scripts, text)
}

/// Runs one site in a child process, with a time limit.
fn child(url: &str) -> Row {
    let failed = |verdict: &str| Row { url: url.into(), verdict: verdict.into(), ..Default::default() };
    let Ok(exe) = std::env::current_exe() else { return failed("couldn't start") };
    let Ok(mut c) = Command::new(exe).args(["survey-one", url]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else { return failed("couldn't start") };
    let started = Instant::now();
    let status = loop {
        match c.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if started.elapsed() > LIMIT => {
                let _ = c.kill();
                let _ = c.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break None,
        }
    };
    let mut out = String::new();
    if let Some(mut o) = c.stdout.take() {
        let _ = o.read_to_string(&mut out);
    }
    match (status, out.lines().last().and_then(Row::from_line)) {
        (_, Some(r)) => r,
        (None, _) => failed("timed out"),
        (Some(s), _) => failed(&format!("crashed ({s})")),
    }
}

fn median(mut v: Vec<f64>, q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * q).round() as usize]
}

/// Error messages with the details that differ between sites taken out (where
/// the script came from, line numbers), so the same failure groups together.
fn normalize(m: &str) -> String {
    let m = match m.find(": ") {
        Some(at) if m.starts_with("http") || m.starts_with("inline script") => &m[at + 2..],
        _ => m,
    };
    let m = match m.find(": line ") {
        Some(at) => &m[..at],
        None => m,
    };
    m.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect()
}

pub fn run(args: &[String]) -> Result<(), String> {
    let list = args.first().ok_or("survey needs a list of sites")?;
    let dir = args.get(1).ok_or("survey needs an output folder")?;
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let jobs: usize = flag("--jobs").and_then(|j| j.parse().ok()).unwrap_or(6);
    let only = flag("--only");
    let text = std::fs::read_to_string(list).map_err(|e| format!("{list}: {e}"))?;
    let urls: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| only.as_ref().map(|o| l.contains(o.as_str())).unwrap_or(true))
        .map(String::from)
        .collect();
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let queue = std::sync::Arc::new(std::sync::Mutex::new(urls.iter().cloned().enumerate().collect::<Vec<_>>()));
    let (tx, rx) = std::sync::mpsc::channel();
    for _ in 0..jobs.max(1) {
        let (queue, tx) = (queue.clone(), tx.clone());
        std::thread::spawn(move || loop {
            let next = queue.lock().unwrap().pop();
            let Some((i, url)) = next else { break };
            let _ = tx.send((i, child(&url)));
        });
    }
    drop(tx);
    let mut rows: Vec<(usize, Row)> = Vec::new();
    for (i, r) in rx {
        eprintln!("[{}/{}] {} — {}", rows.len() + 1, urls.len(), r.url, r.verdict);
        rows.push((i, r));
    }
    rows.sort_by_key(|(i, _)| *i);
    let rows: Vec<Row> = rows.into_iter().map(|(_, r)| r).collect();
    let tsv: String = rows.iter().map(|r| r.to_line() + "\n").collect();
    std::fs::write(format!("{dir}/survey.tsv"), tsv).map_err(|e| e.to_string())?;
    let md = report(&rows, started.elapsed());
    std::fs::write(format!("{dir}/survey.md"), &md).map_err(|e| e.to_string())?;
    println!("{}", md.split("\n## Every site").next().unwrap_or(&md));
    Ok(())
}

fn report(rows: &[Row], took: Duration) -> String {
    let refused = rows.iter().filter(|r| r.verdict.starts_with("refused the survey")).count();
    let n = (rows.len() - refused).max(1);
    let shown = rows.iter().filter(|r| r.verdict == "shown").count();
    let mut out = String::from("## Canard compatibility survey\n\n");
    out += &format!(
        "{} sites in {:.0} s; {refused} turned the survey away (bot protection), leaving {n}. **{shown} of {n} shown by Canard ({:.0}%)**; {} of those had script errors.\n\n",
        rows.len(),
        took.as_secs_f64(),
        shown as f64 * 100.0 / n as f64,
        rows.iter().filter(|r| r.verdict == "shown" && r.errors > 0).count()
    );
    let mut verdicts: HashMap<&str, usize> = HashMap::new();
    for r in rows {
        let v = if r.verdict.starts_with("not a web page") {
            "not a web page"
        } else if r.verdict.starts_with("refused the survey") {
            "refused the survey (bot protection)"
        } else {
            r.verdict.as_str()
        };
        *verdicts.entry(v).or_default() += 1;
    }
    let mut v: Vec<_> = verdicts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    out += "| Outcome | Sites |\n| --- | --- |\n";
    for (k, c) in v {
        out += &format!("| {k} | {c} |\n");
    }
    // Timings over the sites that got as far as running scripts.
    let ran: Vec<&Row> = rows.iter().filter(|r| r.ms[1] > 0.0).collect();
    out += "\n### Time per phase (ms, sites that ran)\n\n| Phase | Median | 90th percentile | Worst |\n| --- | --- | --- | --- |\n";
    for (i, p) in PHASES.iter().enumerate() {
        let v: Vec<f64> = ran.iter().map(|r| r.ms[i]).collect();
        out += &format!("| {p} | {:.0} | {:.0} | {:.0} |\n", median(v.clone(), 0.5), median(v.clone(), 0.9), median(v, 1.0));
    }
    // Uncaught errors, grouped, by how many sites hit them.
    let mut errs: HashMap<String, (usize, String)> = HashMap::new();
    for r in rows {
        let mut seen = std::collections::HashSet::new();
        for m in &r.messages {
            let k = normalize(m);
            if seen.insert(k.clone()) {
                let e = errs.entry(k).or_insert((0, crate::survey::host(&r.url)));
                e.0 += 1;
            }
        }
    }
    let mut errs: Vec<_> = errs.into_iter().collect();
    errs.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.0.cmp(&b.0)));
    out += "\n### Most common script errors\n\n| Sites | Error | e.g. |\n| --- | --- | --- |\n";
    for (k, (c, eg)) in errs.iter().take(30) {
        out += &format!("| {c} | `{}` | {eg} |\n", k.replace('|', "\\|").replace('`', "'"));
    }
    let mut slow: Vec<&&Row> = ran.iter().collect();
    slow.sort_by(|a, b| (b.ms[2] + b.ms[4]).partial_cmp(&(a.ms[2] + a.ms[4])).unwrap());
    out += "\n### Slowest JavaScript\n\n| Site | Scripts | Running (ms) | Downloads (ms) | Timers (ms) | Layout (ms) |\n| --- | --- | --- | --- | --- | --- |\n";
    for r in slow.iter().take(15) {
        out += &format!("| {} | {} | {:.0} | {:.0} | {:.0} | {:.0} |\n", host(&r.url), r.scripts, r.ms[2], r.ms[3], r.ms[4], r.ms[6]);
    }
    out += "\n## Every site\n\n| Site | Outcome | KB | Scripts | Errors | Text | Items | fetch / parse / JS / downloads / timers / css / layout ms |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n";
    for r in rows {
        let ms: Vec<String> = r.ms.iter().map(|m| format!("{m:.0}")).collect();
        out += &format!("| {} | {} | {} | {} | {} | {} | {} | {} |\n", r.url, r.verdict, r.kb, r.scripts, r.errors, r.text, r.items, ms.join(" / "));
    }
    out
}

fn host(url: &str) -> String {
    volant_core::omnibox::host(url).to_string()
}
