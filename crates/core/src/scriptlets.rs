//! uBlock Origin's own scriptlets, as Volant downloads them from uBO's latest
//! release, and the code that runs them on a page.
//!
//! uBO keeps its scriptlets as JavaScript functions (`src/js/resources/`), each
//! registered with a name (`set-constant.js`), aliases (`set.js`), the helper
//! functions it needs (`safe-self.fn`), the world it runs in and whether it
//! needs a trusted list. The app has WebKit load those modules from the
//! release and saves them in the format [`Resources::parse`] reads. For a page,
//! [`Resources::assemble`] builds what uBO itself builds: the functions the
//! site's filters call, plus their helpers, each called once inside `try`.

use crate::json;
use std::collections::HashMap;

/// Separates records in the saved file.
pub const RECORD: char = '\u{1e}';
/// Separates fields in a record.
pub const FIELD: char = '\u{1f}';
/// The first record's first field.
pub const MAGIC: &str = "volant-ubo-scriptlets-1";

#[derive(Debug, Clone, PartialEq)]
pub struct Scriptlet {
    /// `set-constant.js`, or `safe-self.fn` for a helper.
    pub name: String,
    /// The JavaScript function's own name (`setConstant`).
    pub func: String,
    pub code: String,
    pub deps: Vec<String>,
    /// Runs in an isolated world instead of the page's.
    pub isolated: bool,
    /// Needs a trusted list (`trusted-*`).
    pub trust: bool,
}

#[derive(Debug, Default)]
pub struct Resources {
    /// The uBlock Origin release these came from (`1.76.0`).
    pub version: String,
    list: Vec<Scriptlet>,
    /// Name or alias, without `.js` → index.
    by_name: HashMap<String, usize>,
}

/// What to inject for one page.
#[derive(Debug, Default, PartialEq)]
pub struct Assembled {
    /// For the page's own world.
    pub main: String,
    /// For an isolated world.
    pub isolated: String,
    /// Calls to scriptlets this release doesn't have, for Volant's own to try.
    pub unknown: Vec<Vec<String>>,
}

fn bare(name: &str) -> &str {
    name.strip_suffix(".js").unwrap_or(name)
}

fn is_js_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

impl Resources {
    /// Reads the saved file; `None` if it isn't one or holds no scriptlets.
    pub fn parse(text: &str) -> Option<Self> {
        let mut records = text.split(RECORD);
        let head: Vec<&str> = records.next()?.split(FIELD).collect();
        if head.first() != Some(&MAGIC) {
            return None;
        }
        let mut r = Resources { version: head.get(1).unwrap_or(&"").to_string(), ..Default::default() };
        for rec in records {
            let f: Vec<&str> = rec.splitn(7, FIELD).collect();
            let [name, func, world, trust, aliases, deps, code] = f[..] else { continue };
            let c = code.trim_start();
            // Function declarations, and a few helper classes (uBO's JSONPath).
            let declares = c.starts_with("function") || c.starts_with("async function") || c.starts_with("class ");
            if name.is_empty() || !is_js_name(func) || !declares {
                continue;
            }
            let i = r.list.len();
            r.list.push(Scriptlet {
                name: name.to_string(),
                func: func.to_string(),
                code: code.to_string(),
                deps: deps.split(' ').filter(|d| !d.is_empty()).map(String::from).collect(),
                isolated: world.eq_ignore_ascii_case("isolated"),
                trust: trust == "1",
            });
            r.by_name.entry(bare(name).to_string()).or_insert(i);
            for a in aliases.split(' ').filter(|a| !a.is_empty()) {
                r.by_name.entry(bare(a).to_string()).or_insert(i);
            }
        }
        (!r.list.is_empty()).then_some(r)
    }

    pub fn len(&self) -> usize {
        self.list.iter().filter(|s| s.name.ends_with(".js")).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get(&self, name: &str) -> Option<&Scriptlet> {
        self.by_name.get(bare(name.trim())).map(|&i| &self.list[i])
    }

    /// Builds the code for a page's scriptlet calls (`[name, args...]`).
    pub fn assemble(&self, calls: &[Vec<String>]) -> Assembled {
        // Per world: (functions in order, the names already in, the calls).
        #[derive(Default)]
        struct World<'a> {
            funcs: Vec<&'a str>,
            have: Vec<&'a str>,
            calls: String,
        }
        let mut worlds = [World::default(), World::default()];
        let mut out = Assembled::default();
        for call in calls {
            let Some((name, args)) = call.split_first() else { continue };
            let Some(s) = self.get(name).filter(|s| s.name.ends_with(".js")) else {
                out.unknown.push(call.clone());
                continue;
            };
            let w = &mut worlds[s.isolated as usize];
            // The scriptlet and, breadth first, every helper it needs.
            let mut todo = vec![s];
            while let Some(s) = todo.pop() {
                if w.have.contains(&s.name.as_str()) {
                    continue;
                }
                w.have.push(&s.name);
                w.funcs.push(&s.code);
                todo.extend(s.deps.iter().filter_map(|d| self.by_name.get(bare(d)).map(|&i| &self.list[i])));
            }
            let args = args.iter().map(|a| json::string(a)).collect::<Vec<_>>().join(", ");
            w.calls.push_str(&format!("try {{ {}({args}); }} catch (e) {{ }}\n", s.func));
        }
        for (i, w) in worlds.iter().enumerate() {
            if w.calls.is_empty() {
                continue;
            }
            let mut js = String::from("(function uBO_scriptlets() {\nconst scriptletGlobals = {};\n");
            for f in &w.funcs {
                js.push_str(f.trim());
                js.push_str("\n\n");
            }
            js.push_str(&w.calls);
            js.push_str("})();\n");
            if i == 0 {
                out.main = js;
            } else {
                out.isolated = js;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(fields: [&str; 7]) -> String {
        format!("{RECORD}{}", fields.join(&FIELD.to_string()))
    }

    fn sample() -> String {
        let mut t = format!("{MAGIC}{FIELD}1.76.0");
        t += &rec(["safe-self.fn", "safeSelf", "MAIN", "0", "", "", "function safeSelf() { return {}; }"]);
        t += &rec(["set-constant.js", "setConstant", "MAIN", "0", "set.js", "safe-self.fn", "function setConstant(a, b) { safeSelf(); }"]);
        t += &rec(["remove-attr.js", "removeAttr", "ISOLATED", "0", "ra.js", "safe-self.fn", "function removeAttr(a) { safeSelf(); }"]);
        t += &rec(["trusted-set-cookie.js", "trustedSetCookie", "MAIN", "1", "", "", "function trustedSetCookie() {}"]);
        t += &rec(["jsonpath.fn", "JSONPath", "MAIN", "0", "", "", "class JSONPath { }"]);
        t += &rec(["json-edit.js", "jsonEdit", "MAIN", "0", "", "jsonpath.fn", "function jsonEdit() { new JSONPath(); }"]);
        t += &rec(["bad.js", "not a name", "MAIN", "0", "", "", "function x() {}"]);
        t += &rec(["arrow.js", "arrow", "MAIN", "0", "", "", "() => 1"]);
        t
    }

    #[test]
    fn parses_and_assembles() {
        let r = Resources::parse(&sample()).unwrap();
        assert_eq!(r.version, "1.76.0");
        assert_eq!(r.len(), 4, "helpers and bad records don't count");
        assert!(r.assemble(&[vec!["json-edit".into()]]).main.contains("class JSONPath { }"));
        assert!(r.get("trusted-set-cookie").unwrap().trust);
        let calls = vec![
            vec!["set".to_string(), "a.b".into(), "true".into()],
            vec!["set-constant.js".to_string(), "c".into(), "\"q\"".into()],
            vec!["ra".to_string(), "onclick".into()],
            vec!["volant-only".to_string()],
        ];
        let a = r.assemble(&calls);
        assert_eq!(a.main.matches("function safeSelf").count(), 1, "each helper once");
        assert_eq!(a.main.matches("function setConstant").count(), 1);
        assert!(a.main.contains(r#"try { setConstant("a.b", "true"); } catch (e) { }"#));
        assert!(a.main.contains(r#"setConstant("c", "\"q\"")"#));
        assert!(a.main.starts_with("(function uBO_scriptlets() {\nconst scriptletGlobals = {};"));
        assert!(a.isolated.contains("function removeAttr") && a.isolated.contains("function safeSelf"));
        assert!(!a.main.contains("removeAttr"));
        assert_eq!(a.unknown, vec![vec!["volant-only".to_string()]]);
        assert_eq!(r.assemble(&[]), Assembled::default());
    }

    #[test]
    fn rejects_other_files() {
        assert!(Resources::parse("").is_none());
        assert!(Resources::parse("{\"json\":1}").is_none());
        assert!(Resources::parse(&format!("{MAGIC}{FIELD}1.0")).is_none(), "no scriptlets");
    }
}
