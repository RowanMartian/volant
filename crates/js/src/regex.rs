//! Regular expressions with JavaScript's syntax and semantics, from scratch:
//! a parser to a small syntax tree and a backtracking matcher.
//!
//! Supported: alternation, greedy and lazy quantifiers (`* + ? {n,m}`),
//! capturing, non-capturing and named groups, back-references (`\1`,
//! `\k<name>`), character classes with ranges and the `\d \w \s` escapes,
//! anchors, word boundaries, lookahead and lookbehind, `\p{…}` for the common
//! Unicode categories, and the flags `g i m s u y` (plus `d`, accepted).
//! Positions are counted in characters. A step budget stops catastrophic
//! backtracking: a pattern that runs out of steps simply doesn't match.

#[derive(Debug, Clone)]
enum Node {
    Empty,
    Char(char),
    Any,
    Class(Vec<Item>, bool),
    Start,
    End,
    WordBoundary(bool),
    Group(Box<Node>, Option<usize>),
    Alt(Vec<Node>),
    Seq(Vec<Node>),
    Repeat(Box<Node>, u32, u32, bool),
    Back(usize),
    NamedBack(String),
    Look(Box<Node>, bool, bool),
}

#[derive(Debug, Clone)]
enum Item {
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
    Prop(String, bool),
}

#[derive(Debug)]
pub struct Regex {
    node: Node,
    /// The number of capturing groups.
    pub groups: usize,
    pub names: Vec<(String, usize)>,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub global: bool,
    pub sticky: bool,
    pub unicode: bool,
}

/// Capture positions: index 0 is the whole match.
pub type Captures = Vec<Option<(usize, usize)>>;

const STEP_LIMIT: u64 = 20_000_000;

struct Parser<'a> {
    s: &'a [char],
    i: usize,
    groups: usize,
    names: Vec<(String, usize)>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn alt(&mut self) -> Result<Node, String> {
        let mut options = vec![self.seq()?];
        while self.eat('|') {
            options.push(self.seq()?);
        }
        Ok(if options.len() == 1 { options.pop().unwrap_or(Node::Empty) } else { Node::Alt(options) })
    }

    fn seq(&mut self) -> Result<Node, String> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            let atom = self.quantifier(atom)?;
            items.push(atom);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().unwrap_or(Node::Empty),
            _ => Node::Seq(items),
        })
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.i;
        while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            self.i += 1;
        }
        if start == self.i {
            return None;
        }
        self.s[start..self.i].iter().collect::<String>().parse().ok().or(Some(u32::MAX))
    }

    fn quantifier(&mut self, atom: Node) -> Result<Node, String> {
        let (min, max) = match self.peek() {
            Some('*') => (0, u32::MAX),
            Some('+') => (1, u32::MAX),
            Some('?') => (0, 1),
            Some('{') => {
                let save = self.i;
                self.i += 1;
                let Some(min) = self.number() else {
                    self.i = save;
                    return Ok(atom);
                };
                let max = if self.eat(',') { self.number().unwrap_or(u32::MAX) } else { min };
                if !self.eat('}') {
                    self.i = save;
                    return Ok(atom);
                }
                self.i -= 1; // re-consumed below
                if max < min {
                    return Err("numbers out of order in {} quantifier".into());
                }
                (min, max)
            }
            _ => return Ok(atom),
        };
        self.i += 1;
        if matches!(atom, Node::Start | Node::End | Node::WordBoundary(_) | Node::Look(_, false, _)) {
            return Err("nothing to repeat".into());
        }
        let greedy = !self.eat('?');
        // A repeated lookahead (allowed on the web): it matches no text, so once is as many as matter.
        let (min, max) = if matches!(atom, Node::Look(..)) { (min.min(1), max.min(1)) } else { (min, max) };
        Ok(Node::Repeat(Box::new(atom), min, max, greedy))
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.peek().ok_or("unexpected end")?;
        self.i += 1;
        Ok(match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                let mut capture = true;
                let mut look = None;
                let mut name = None;
                if self.eat('?') {
                    if self.eat(':') {
                        capture = false;
                    } else if self.eat('=') {
                        look = Some((true, false));
                    } else if self.eat('!') {
                        look = Some((true, true));
                    } else if self.eat('<') {
                        if self.eat('=') {
                            look = Some((false, false));
                        } else if self.eat('!') {
                            look = Some((false, true));
                        } else {
                            let start = self.i;
                            while self.peek().map(|c| c != '>').unwrap_or(false) {
                                self.i += 1;
                            }
                            name = Some(self.s[start..self.i].iter().collect::<String>());
                            if !self.eat('>') {
                                return Err("unterminated group name".into());
                            }
                        }
                    } else {
                        return Err("invalid group".into());
                    }
                }
                let index = if capture && look.is_none() {
                    self.groups += 1;
                    let g = self.groups;
                    if let Some(n) = name {
                        self.names.push((n, g));
                    }
                    Some(g)
                } else {
                    None
                };
                let inner = self.alt()?;
                if !self.eat(')') {
                    return Err("unterminated group".into());
                }
                match look {
                    Some((ahead, neg)) => Node::Look(Box::new(inner), ahead, neg),
                    None => Node::Group(Box::new(inner), index),
                }
            }
            ')' => return Err("unmatched ')'".into()),
            '[' => self.class()?,
            '\\' => self.escape(false)?,
            '*' | '+' | '?' => return Err("nothing to repeat".into()),
            c => Node::Char(c),
        })
    }

    fn hex(&mut self, n: usize) -> Option<char> {
        let digits: String = self.s.get(self.i..self.i + n)?.iter().collect();
        let ch = char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?;
        self.i += n;
        Some(ch)
    }

    /// After a backslash. Returns a `Node::Class` for class escapes.
    fn escape(&mut self, in_class: bool) -> Result<Node, String> {
        let c = self.peek().ok_or("\\ at end of pattern")?;
        self.i += 1;
        let class = |item: Item| Node::Class(vec![item], false);
        Ok(match c {
            'd' => class(Item::Digit(true)),
            'D' => class(Item::Digit(false)),
            'w' => class(Item::Word(true)),
            'W' => class(Item::Word(false)),
            's' => class(Item::Space(true)),
            'S' => class(Item::Space(false)),
            'b' if in_class => Node::Char('\u{8}'),
            'b' => Node::WordBoundary(true),
            'B' => Node::WordBoundary(false),
            'n' => Node::Char('\n'),
            'r' => Node::Char('\r'),
            't' => Node::Char('\t'),
            'v' => Node::Char('\u{b}'),
            'f' => Node::Char('\u{c}'),
            '0' if !self.peek().map(|d| d.is_ascii_digit()).unwrap_or(false) => Node::Char('\0'),
            'c' => match self.peek() {
                Some(l) if l.is_ascii_alphabetic() => {
                    self.i += 1;
                    Node::Char(char::from_u32(l as u32 % 32).unwrap_or('\0'))
                }
                _ => Node::Char('\\'),
            },
            'x' => Node::Char(self.hex(2).unwrap_or('x')),
            'u' => {
                if self.eat('{') {
                    let start = self.i;
                    while self.peek().map(|c| c != '}').unwrap_or(false) {
                        self.i += 1;
                    }
                    let digits: String = self.s[start..self.i].iter().collect();
                    self.eat('}');
                    Node::Char(u32::from_str_radix(&digits, 16).ok().and_then(char::from_u32).unwrap_or('\u{fffd}'))
                } else {
                    match self.hex(4) {
                        // A surrogate pair written as two escapes.
                        Some(h) => Node::Char(h),
                        None => {
                            let save = self.i;
                            let digits: String = self.s.get(self.i..self.i + 4).map(|d| d.iter().collect()).unwrap_or_default();
                            match u32::from_str_radix(&digits, 16) {
                                Ok(hi) if (0xD800..0xDC00).contains(&hi) && self.s.get(self.i + 4) == Some(&'\\') && self.s.get(self.i + 5) == Some(&'u') => {
                                    self.i += 6;
                                    let lo: String = self.s.get(self.i..self.i + 4).map(|d| d.iter().collect()).unwrap_or_default();
                                    self.i += 4;
                                    let lo = u32::from_str_radix(&lo, 16).unwrap_or(0xDC00);
                                    Node::Char(char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3ff)).unwrap_or('\u{fffd}'))
                                }
                                Ok(_) => {
                                    self.i = save + 4;
                                    Node::Char('\u{fffd}')
                                }
                                Err(_) => Node::Char('u'),
                            }
                        }
                    }
                }
            }
            'p' | 'P' if self.peek() == Some('{') => {
                self.i += 1;
                let start = self.i;
                while self.peek().map(|c| c != '}').unwrap_or(false) {
                    self.i += 1;
                }
                let name: String = self.s[start..self.i].iter().collect();
                self.eat('}');
                class(Item::Prop(name, c == 'p'))
            }
            'k' if !in_class && self.peek() == Some('<') => {
                self.i += 1;
                let start = self.i;
                while self.peek().map(|c| c != '>').unwrap_or(false) {
                    self.i += 1;
                }
                let name: String = self.s[start..self.i].iter().collect();
                self.eat('>');
                Node::NamedBack(name)
            }
            '1'..='9' if !in_class => {
                self.i -= 1;
                Node::Back(self.number().unwrap_or(0) as usize)
            }
            c => Node::Char(c),
        })
    }

    fn class(&mut self) -> Result<Node, String> {
        let negated = self.eat('^');
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let c = self.peek().ok_or("unterminated character class")?;
            if c == ']' && !first {
                self.i += 1;
                break;
            }
            if c == ']' {
                // `[]` matches nothing; `[^]` matches anything.
                self.i += 1;
                break;
            }
            first = false;
            self.i += 1;
            let lo = if c == '\\' {
                match self.escape(true)? {
                    Node::Char(ch) => ch,
                    Node::Class(mut it, _) => {
                        items.append(&mut it);
                        continue;
                    }
                    _ => continue,
                }
            } else {
                c
            };
            // A range a-z (but not a trailing '-').
            if self.peek() == Some('-') && self.s.get(self.i + 1).map(|&n| n != ']').unwrap_or(false) {
                self.i += 1;
                let h = self.peek().ok_or("unterminated character class")?;
                self.i += 1;
                let hi = if h == '\\' {
                    match self.escape(true)? {
                        Node::Char(ch) => ch,
                        Node::Class(mut it, _) => {
                            items.push(Item::Range(lo, lo));
                            items.push(Item::Range('-', '-'));
                            items.append(&mut it);
                            continue;
                        }
                        _ => continue,
                    }
                } else {
                    h
                };
                if hi < lo {
                    return Err("range out of order in character class".into());
                }
                items.push(Item::Range(lo, hi));
            } else {
                items.push(Item::Range(lo, lo));
            }
        }
        Ok(Node::Class(items, negated))
    }
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}' || c == '\u{a0}'
}

fn is_line_end(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn prop(name: &str, c: char) -> bool {
    let name = name.split_once('=').map(|x| x.1).unwrap_or(name);
    match name {
        "L" | "Letter" | "Alphabetic" | "Alpha" => c.is_alphabetic(),
        "Lu" | "Uppercase_Letter" | "Uppercase" => c.is_uppercase(),
        "Ll" | "Lowercase_Letter" | "Lowercase" => c.is_lowercase(),
        "N" | "Number" | "Nd" | "Decimal_Number" | "digit" => c.is_numeric(),
        "P" | "Punctuation" => c.is_ascii_punctuation() || matches!(c, '\u{2010}'..='\u{2027}' | '\u{3000}'..='\u{303f}'),
        "Z" | "Separator" | "Zs" | "Space_Separator" | "White_Space" => is_space(c),
        "Emoji" | "Emoji_Presentation" | "Extended_Pictographic" => matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF),
        "Script=Latin" | "Latin" => c.is_ascii_alphabetic() || matches!(c as u32, 0xC0..=0x24F),
        "Han" => matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF),
        "Cyrillic" => matches!(c as u32, 0x400..=0x4FF),
        "Greek" => matches!(c as u32, 0x370..=0x3FF),
        "Arabic" => matches!(c as u32, 0x600..=0x6FF),
        "Hebrew" => matches!(c as u32, 0x590..=0x5FF),
        "C" | "Other" | "Cc" | "Control" => c.is_control(),
        "M" | "Mark" | "Mn" => matches!(c as u32, 0x300..=0x36F | 0x1AB0..=0x1AFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F),
        "S" | "Symbol" | "Sm" | "Sc" => matches!(c, '$' | '+' | '<' | '=' | '>' | '^' | '`' | '|' | '~' | '€' | '£' | '¥' | '©' | '®') || matches!(c as u32, 0x2100..=0x2BFF),
        "ASCII" => c.is_ascii(),
        "Any" => true,
        _ => c.is_alphanumeric(),
    }
}

struct State<'a> {
    s: &'a [char],
    caps: Captures,
    steps: u64,
}

impl Regex {
    pub fn new(pattern: &str, flags: &str) -> Result<Regex, String> {
        for f in flags.chars() {
            if !"gimsuyd".contains(f) || flags.matches(f).count() > 1 {
                return Err(format!("invalid regular expression flags '{flags}'"));
            }
        }
        let chars: Vec<char> = pattern.chars().collect();
        let mut p = Parser { s: &chars, i: 0, groups: 0, names: Vec::new() };
        let mut node = p.alt().map_err(|e| format!("invalid regular expression /{pattern}/: {e}"))?;
        if p.i < chars.len() {
            return Err(format!("invalid regular expression /{pattern}/: unmatched ')'"));
        }
        let names = p.names;
        resolve_names(&mut node, &names);
        Ok(Regex {
            node,
            groups: p.groups,
            names,
            ignore_case: flags.contains('i'),
            multiline: flags.contains('m'),
            dot_all: flags.contains('s'),
            global: flags.contains('g'),
            sticky: flags.contains('y'),
            unicode: flags.contains('u'),
        })
    }

    /// The first match starting at or after `start` (exactly at it when sticky).
    pub fn exec_at(&self, s: &[char], start: usize) -> Option<Captures> {
        let mut st = State { s, caps: vec![None; self.groups + 1], steps: 0 };
        let last = if self.sticky { start.min(s.len()) } else { s.len() };
        let mut pos = start;
        while pos <= last {
            for c in st.caps.iter_mut() {
                *c = None;
            }
            let found = self.m(&self.node, pos, &mut st, &mut |e, st| {
                st.caps[0] = Some((pos, e));
                true
            });
            if found {
                return Some(st.caps);
            }
            if st.steps > STEP_LIMIT {
                return None;
            }
            pos += 1;
        }
        None
    }

    pub fn is_match(&self, s: &str) -> bool {
        let chars: Vec<char> = s.chars().collect();
        self.exec_at(&chars, 0).is_some()
    }

    fn eq(&self, a: char, b: char) -> bool {
        a == b || (self.ignore_case && fold(a) == fold(b))
    }

    fn in_class(&self, items: &[Item], negated: bool, c: char) -> bool {
        let hit = items.iter().any(|it| match it {
            Item::Range(lo, hi) => {
                (*lo <= c && c <= *hi)
                    || (self.ignore_case && {
                        let (l, u) = (fold(c), c.to_uppercase().next().unwrap_or(c));
                        (*lo <= l && l <= *hi) || (*lo <= u && u <= *hi)
                    })
            }
            Item::Digit(yes) => c.is_ascii_digit() == *yes,
            Item::Word(yes) => is_word(c) == *yes,
            Item::Space(yes) => is_space(c) == *yes,
            Item::Prop(name, yes) => prop(name, c) == *yes,
        });
        hit != negated
    }

    /// Whether a single-character node matches `c` (None if the node isn't one).
    fn one(&self, n: &Node, c: char) -> Option<bool> {
        Some(match n {
            Node::Char(x) => self.eq(*x, c),
            Node::Any => self.dot_all || !is_line_end(c),
            Node::Class(items, neg) => self.in_class(items, *neg, c),
            _ => return None,
        })
    }

    fn m(&self, n: &Node, i: usize, st: &mut State, k: &mut dyn FnMut(usize, &mut State) -> bool) -> bool {
        st.steps += 1;
        if st.steps > STEP_LIMIT {
            return false;
        }
        match n {
            Node::Empty => k(i, st),
            Node::Char(_) | Node::Any | Node::Class(..) => match st.s.get(i) {
                Some(&c) if self.one(n, c) == Some(true) => k(i + 1, st),
                _ => false,
            },
            Node::Start => (i == 0 || (self.multiline && is_line_end(st.s[i - 1]))) && k(i, st),
            Node::End => (i == st.s.len() || (self.multiline && is_line_end(st.s[i]))) && k(i, st),
            Node::WordBoundary(want) => {
                let a = i > 0 && is_word(st.s[i - 1]);
                let b = i < st.s.len() && is_word(st.s[i]);
                ((a != b) == *want) && k(i, st)
            }
            Node::Seq(items) => self.seq(items, i, st, k),
            Node::Alt(options) => options.iter().any(|o| self.m(o, i, st, k)),
            Node::Group(inner, None) => self.m(inner, i, st, k),
            Node::Group(inner, Some(g)) => {
                let g = *g;
                self.m(inner, i, st, &mut |j, st| {
                    let old = st.caps[g];
                    st.caps[g] = Some((i, j));
                    if k(j, st) {
                        true
                    } else {
                        st.caps[g] = old;
                        false
                    }
                })
            }
            Node::Repeat(inner, min, max, greedy) => {
                if inner_is_single(inner) {
                    // Fast path: count how far it can run, then try each length.
                    let mut n = 0usize;
                    while (n as u32) < *max && st.s.get(i + n).map(|&c| self.one(inner, c) == Some(true)).unwrap_or(false) {
                        n += 1;
                    }
                    let min = *min as usize;
                    if n < min {
                        return false;
                    }
                    if *greedy {
                        (min..=n).rev().any(|len| k(i + len, st))
                    } else {
                        (min..=n).any(|len| k(i + len, st))
                    }
                } else {
                    self.rep(inner, *min, *max, *greedy, 0, i, st, k)
                }
            }
            Node::Back(g) => {
                let Some(Some((a, b))) = st.caps.get(*g).copied() else { return k(i, st) };
                let len = b - a;
                if i + len > st.s.len() {
                    return false;
                }
                for d in 0..len {
                    if !self.eq(st.s[a + d], st.s[i + d]) {
                        return false;
                    }
                }
                k(i + len, st)
            }
            Node::NamedBack(_) => k(i, st),
            Node::Look(inner, ahead, neg) => {
                let saved = st.caps.clone();
                let found = if *ahead {
                    self.m(inner, i, st, &mut |_, _| true)
                } else {
                    (0..=i).rev().any(|from| self.m(inner, from, st, &mut |e, _| e == i))
                };
                if *neg {
                    st.caps = saved;
                    !found && k(i, st)
                } else if !found {
                    st.caps = saved;
                    false
                } else if k(i, st) {
                    true
                } else {
                    st.caps = saved;
                    false
                }
            }
        }
    }

    fn seq(&self, items: &[Node], i: usize, st: &mut State, k: &mut dyn FnMut(usize, &mut State) -> bool) -> bool {
        match items.split_first() {
            None => k(i, st),
            Some((first, [])) => self.m(first, i, st, k),
            Some((first, rest)) => self.m(first, i, st, &mut |j, st| self.seq(rest, j, st, k)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rep(&self, inner: &Node, min: u32, max: u32, greedy: bool, count: u32, i: usize, st: &mut State, k: &mut dyn FnMut(usize, &mut State) -> bool) -> bool {
        let again = |st: &mut State, k: &mut dyn FnMut(usize, &mut State) -> bool| {
            count < max
                && self.m(inner, i, st, &mut |j, st| {
                    // An empty iteration can't make progress: stop repeating.
                    if j == i && count >= min {
                        return false;
                    }
                    self.rep(inner, min, max, greedy, count.saturating_add(1), j, st, k)
                })
        };
        if greedy {
            again(st, k) || (count >= min && k(i, st))
        } else {
            (count >= min && k(i, st)) || again(st, k)
        }
    }
}

fn inner_is_single(n: &Node) -> bool {
    matches!(n, Node::Char(_) | Node::Any | Node::Class(..))
}

fn resolve_names(n: &mut Node, names: &[(String, usize)]) {
    match n {
        Node::NamedBack(name) => {
            if let Some((_, g)) = names.iter().find(|(x, _)| x == name) {
                *n = Node::Back(*g);
            }
        }
        Node::Group(inner, _) | Node::Repeat(inner, ..) | Node::Look(inner, ..) => resolve_names(inner, names),
        Node::Seq(items) | Node::Alt(items) => items.iter_mut().for_each(|x| resolve_names(x, names)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(p: &str, flags: &str, s: &str) -> Option<String> {
        let re = Regex::new(p, flags).unwrap();
        let chars: Vec<char> = s.chars().collect();
        re.exec_at(&chars, 0).map(|c| {
            let (a, b) = c[0].unwrap();
            chars[a..b].iter().collect()
        })
    }

    #[test]
    fn matching() {
        assert_eq!(find("b+", "", "aabbbc").as_deref(), Some("bbb"));
        assert_eq!(find("b+?", "", "aabbbc").as_deref(), Some("b"));
        assert_eq!(find("^\\d{3}-\\d{4}$", "", "555-1234").as_deref(), Some("555-1234"));
        assert_eq!(find("colou?r", "i", "The COLOR red").as_deref(), Some("COLOR"));
        assert_eq!(find("(\\w+)\\s\\1", "", "say hello hello there").as_deref(), Some("hello hello"));
        assert_eq!(find("(?<y>\\d{4})-\\k<y>", "", "x 2026-2026").as_deref(), Some("2026-2026"));
        assert_eq!(find("\\bcat\\b", "", "concat cat").as_deref(), Some("cat"));
        assert_eq!(find("foo(?=bar)", "", "foobaz foobar").as_deref(), Some("foo"));
        assert_eq!(find("(?<!\\$)\\b\\d+", "", "$5 and 7").as_deref(), Some("7"));
        assert_eq!(find("[a-f\\d]+", "", "zz09afz").as_deref(), Some("09af"));
        assert_eq!(find("[^aeiou\\s]+", "", "aei xyz").as_deref(), Some("xyz"));
        assert_eq!(find("a|ab|abc", "", "abc").as_deref(), Some("a"));
        assert_eq!(find("(a|ab)(c|bcd)(d*)", "", "abcd").as_deref(), Some("abcd"));
        assert_eq!(find("^b", "m", "a\nb").as_deref(), Some("b"));
        assert_eq!(find("a.c", "", "a\nc abc").as_deref(), Some("abc"));
        assert_eq!(find("a.c", "s", "a\nc").as_deref(), Some("a\nc"));
        assert_eq!(find("\\p{Lu}\\p{Ll}+", "u", "hello Émile").as_deref(), Some("Émile"));
        assert_eq!(find("(?:ab){2,}", "", "abababx").as_deref(), Some("ababab"));
        assert_eq!(find("x*", "", "abc").as_deref(), Some(""));
        assert!(Regex::new("(", "").is_err() && Regex::new("a", "gg").is_err() && Regex::new("*a", "").is_err());
        // Catastrophic patterns give up instead of hanging.
        let t = std::time::Instant::now();
        assert!(find("(a+)+$", "", &format!("{}b", "a".repeat(40))).is_none());
        assert!(t.elapsed().as_secs() < 20);
    }
}
