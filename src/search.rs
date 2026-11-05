//! Find / replace over a buffer and find-in-files over a folder tree.

use regex_lite::{Regex, RegexBuilder};
use std::path::Path;

#[derive(Clone, Default)]
pub struct Query {
    pub pat: String,
    pub case: bool,
    pub word: bool,
    pub regex: bool,
}

pub struct Matcher {
    re: Regex,
    /// Plain text (not a regex): searched directly, many times faster.
    lit: Option<Lit>,
}

/// A literal search: the first byte is found with the C runtime's
/// vectorised memchr and the rest compared in place.
struct Lit {
    /// Lower-cased when the case is ignored (ASCII only).
    needle: Vec<u8>,
    ci: bool,
    word: bool,
}

/// A word character as the regex engine's `\b` sees it (ASCII).
fn word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

impl Lit {
    /// Whether the needle matches at `i`; `before` / `after` are the bytes
    /// just outside `h` (it may be one piece of a longer text), for the
    /// whole-word test.
    fn at(&self, h: &[u8], i: usize, before: Option<u8>, after: Option<u8>) -> bool {
        let n = self.needle.len();
        if i + n > h.len() {
            return false;
        }
        let hit = if self.ci { h[i..i + n].eq_ignore_ascii_case(&self.needle) } else { h[i..i + n] == self.needle[..] };
        // Whole word: a word boundary at both ends, exactly like `\b`.
        let prev = if i == 0 { before } else { Some(h[i - 1]) };
        let next = if i + n == h.len() { after } else { Some(h[i + n]) };
        let wb = |c: Option<u8>| c.map_or(false, word_byte);
        hit && (!self.word || (wb(prev) != wb(Some(h[i])) && wb(Some(h[i + n - 1])) != wb(next)))
    }

    /// First match starting at or after `from`.
    fn find(&self, h: &[u8], from: usize) -> Option<usize> {
        self.find_ctx(h, from, None, None)
    }

    /// Every match start in `h` from `from` (non-overlapping), with the
    /// bytes outside `h`, until `f` says stop.
    fn each(&self, h: &[u8], from: usize, before: Option<u8>, after: Option<u8>, mut f: impl FnMut(usize) -> bool) {
        let mut i = from;
        while let Some(c) = self.find_ctx(h, i, before, after) {
            if !f(c) {
                return;
            }
            i = c + self.needle.len();
        }
    }

    fn find_ctx(&self, h: &[u8], from: usize, before: Option<u8>, after: Option<u8>) -> Option<usize> {
        let n0 = self.needle[0];
        let other = if self.ci && n0.is_ascii_alphabetic() { Some(n0.to_ascii_uppercase()) } else { None };
        // Next position of each spelling of the first byte, kept between
        // candidates so neither is searched twice.
        let next = |b: u8, i: usize| crate::diff::find(h.get(i..).unwrap_or(&[]), b).map(|k| i + k);
        let (mut lo, mut up) = (next(n0, from), other.and_then(|u| next(u, from)));
        loop {
            let c = match (lo, up) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => return None,
            };
            if c + self.needle.len() > h.len() {
                return None;
            }
            if self.at(h, c, before, after) {
                return Some(c);
            }
            if lo == Some(c) {
                lo = next(n0, c + 1);
            }
            if up == Some(c) {
                up = other.and_then(|u| next(u, c + 1));
            }
        }
    }
}

impl Query {
    pub fn compile(&self) -> Result<Matcher, String> {
        if self.pat.is_empty() {
            return Err(String::new());
        }
        let mut p = if self.regex { self.pat.clone() } else { regex_lite::escape(&self.pat) };
        if self.word {
            p = format!(r"\b(?:{p})\b");
        }
        // Plain text goes the fast way; ignoring case beyond ASCII needs the
        // regex engine's case folding.
        let lit = (!self.regex && (self.case || self.pat.is_ascii())).then(|| Lit {
            needle: if self.case { self.pat.as_bytes().to_vec() } else { self.pat.to_ascii_lowercase().into_bytes() },
            ci: !self.case,
            word: self.word,
        });
        RegexBuilder::new(&p)
            .case_insensitive(!self.case)
            .multi_line(true)
            .build()
            .map(|re| Matcher { re, lit })
            .map_err(|e| e.to_string().lines().last().unwrap_or("bad pattern").to_string())
    }
}

impl Matcher {
    /// Plain text, searched the fast way.
    pub fn is_literal(&self) -> bool {
        self.lit.is_some()
    }

    pub fn find_all(&self, text: &str, limit: usize) -> Vec<(usize, usize)> {
        if let Some(l) = &self.lit {
            let (h, n) = (text.as_bytes(), l.needle.len());
            let mut out = Vec::new();
            let mut i = 0;
            while out.len() < limit {
                let Some(c) = l.find(h, i) else { break };
                out.push((c, c + n));
                i = c + n;
            }
            return out;
        }
        self.re.find_iter(text).filter(|m| m.end() > m.start()).take(limit).map(|m| (m.start(), m.end())).collect()
    }

    pub fn find_from(&self, text: &str, from: usize) -> Option<(usize, usize)> {
        if let Some(l) = &self.lit {
            return l.find(text.as_bytes(), from).map(|c| (c, c + l.needle.len()));
        }
        let mut at = from;
        while at <= text.len() {
            let m = self.re.find_at(text, at)?;
            if m.end() > m.start() {
                return Some((m.start(), m.end()));
            }
            at = m.end() + 1;
            while at < text.len() && !text.is_char_boundary(at) {
                at += 1;
            }
        }
        None
    }

    pub fn find_before(&self, text: &str, before: usize) -> Option<(usize, usize)> {
        if let Some(l) = &self.lit {
            let h = text.as_bytes();
            let (mut last, mut i) = (None, 0);
            while let Some(c) = l.find(h, i) {
                if c >= before {
                    break;
                }
                last = Some((c, c + l.needle.len()));
                // Non-overlapping, as the regex finds them.
                i = c + l.needle.len();
            }
            return last;
        }
        let mut last = None;
        for m in self.re.find_iter(text) {
            if m.start() >= before {
                break;
            }
            if m.end() > m.start() {
                last = Some((m.start(), m.end()));
            }
        }
        last
    }

    /// Replacement text for the match at `range`, expanding `$1` in regex mode.
    pub fn expand(&self, q: &Query, text: &str, range: (usize, usize), repl: &str) -> String {
        if !q.regex {
            return repl.to_string();
        }
        let repl = repl.replace("\\n", "\n").replace("\\t", "\t");
        match self.re.captures_at(text, range.0) {
            Some(c) if c.get(0).map(|m| m.start()) == Some(range.0) => {
                let mut s = String::new();
                c.expand(&repl, &mut s);
                s
            }
            _ => repl,
        }
    }
}

/// Texts this big are searched in windows rather than as one string, so a
/// search never copies or re-validates the whole document (and never moves
/// the gap across it).
pub const WINDOWED: usize = 64 << 20;
const WIN: usize = 8 << 20;
/// Matches longer than this can be missed at a window edge in huge files.
const OVERLAP: usize = 64 << 10;

/// Start of the character at or after `p`.
fn char_start(doc: &crate::doc::Doc, mut p: usize) -> usize {
    let n = doc.len();
    while p < n && doc.byte(p) & 0xC0 == 0x80 {
        p += 1;
    }
    p
}

/// The next match at or after `from` (`forward`), or the last one before
/// it, wrapping round the end. `limit` caps the bytes looked at (for
/// search-as-you-type in huge files); None searches everything.
pub fn doc_find(doc: &mut crate::doc::Doc, m: &Matcher, from: usize, forward: bool, limit: Option<usize>) -> Option<(usize, usize)> {
    if let Some(l) = m.lit.as_ref().filter(|_| doc.len() >= WINDOWED) {
        return lit_find_doc(doc, l, from.min(doc.len()), forward, limit);
    }
    doc_find_with(doc, m, from, forward, limit, WINDOWED, WIN)
}

/// Plain-text search straight in the document's memory: the two pieces
/// around the gap are searched in place and only the few bytes where a
/// match could straddle the gap are copied, so a gigabyte takes about a
/// second.
fn lit_find_doc(doc: &crate::doc::Doc, l: &Lit, from: usize, forward: bool, limit: Option<usize>) -> Option<(usize, usize)> {
    let n = doc.len();
    let k = l.needle.len();
    let [a, b] = doc.gap.parts();
    let gs = a.len();
    let byte = |p: usize| (p < n).then(|| doc.byte(p));
    // Calls `f` with each match starting in [s, e), in order, until it
    // returns false: piece A, then the seam across the gap, then piece B.
    let scan = |s: usize, e: usize, f: &mut dyn FnMut(usize) -> bool| {
        if s >= e {
            return;
        }
        let mut go = true;
        if s < gs {
            let end = (e + k - 1).min(gs);
            l.each(&a[..end], s, None, byte(end), |c| {
                go = c < e && f(c);
                go
            });
            if !go {
                return;
            }
        }
        if gs > 0 && gs < n && k > 1 {
            let lo = gs.saturating_sub(k - 1).max(s);
            let hi = (gs + k - 1).min(n);
            if lo < gs.min(e) {
                let seam = doc.text(lo, hi);
                let mut i = 0;
                while let Some(c) = l.find_ctx(&seam, i, lo.checked_sub(1).map(|q| doc.byte(q)), byte(hi)) {
                    let at = lo + c;
                    if at >= gs || at >= e {
                        break;
                    }
                    // Only matches that really cross the gap; the others
                    // were found in piece A.
                    if at + k > gs && !f(at) {
                        return;
                    }
                    i = c + 1;
                }
            }
        }
        if e > gs {
            let start = s.max(gs) - gs;
            let end = (e - gs + k - 1).min(b.len());
            l.each(&b[..end], start, gs.checked_sub(1).map(|q| doc.byte(q)), byte(gs + end), |c| f(gs + c) && gs + c < e);
        }
    };
    let span = limit.unwrap_or(usize::MAX);
    if forward {
        let mut hit = None;
        let mut first = |c: usize| {
            hit = Some(c);
            false
        };
        scan(from, n.min(from.saturating_add(span)), &mut first);
        if hit.is_none() && limit.is_none() {
            scan(0, from, &mut |c| {
                hit = Some(c);
                false
            });
        }
        hit.map(|c| (c, c + k))
    } else {
        // The last match before `from`, else (wrapping) the last one of all.
        // Stretches are searched backwards so the nearest match ends it.
        const STRETCH: usize = 16 << 20;
        let last_in = |s: usize, e: usize| {
            let mut hi = e;
            while hi > s {
                let lo = hi.saturating_sub(STRETCH).max(s);
                let mut last = None;
                scan(lo, hi, &mut |c| {
                    if c < hi {
                        last = Some(c);
                    }
                    true
                });
                if last.is_some() {
                    return last;
                }
                hi = lo;
            }
            None
        };
        let mut hit = last_in(from.saturating_sub(span), from);
        if hit.is_none() && limit.is_none() {
            hit = last_in(from, n);
        }
        hit.map(|c| (c, c + k))
    }
}

/// `doc_find` with the window sizes given (tests use small ones).
fn doc_find_with(doc: &mut crate::doc::Doc, m: &Matcher, from: usize, forward: bool, limit: Option<usize>, windowed: usize, win: usize) -> Option<(usize, usize)> {
    let n = doc.len();
    let from = from.min(n);
    if n < windowed {
        let text = String::from_utf8_lossy(doc.gap.contiguous());
        return if forward {
            m.find_from(&text, from).or_else(|| if limit.is_none() { m.find_from(&text, 0) } else { None })
        } else {
            m.find_before(&text, from).or_else(|| if limit.is_none() { m.find_before(&text, text.len() + 1) } else { None })
        };
    }
    let overlap = OVERLAP.min(win / 4);
    let budget = limit.unwrap_or(usize::MAX);
    // Text of [s, e) cut at character starts, and where it begins.
    let window = |doc: &crate::doc::Doc, s: usize, e: usize| -> (usize, usize, String) {
        let (s, e) = (char_start(doc, s), char_start(doc, e.min(n)));
        (s, e, String::from_utf8_lossy(&doc.text(s, e)).into_owned())
    };
    let mut seen = 0usize;
    if forward {
        // Matches starting in [from, n), then (wrapping) in [0, from).
        let mut ranges = vec![(from, n)];
        if limit.is_none() {
            ranges.push((0, from));
        }
        for (a, z) in ranges {
            let mut s = a;
            while s < z && seen < budget {
                let (ws, we, text) = window(doc, s, s + win);
                match m.find_from(&text, s.saturating_sub(ws)).map(|(x, y)| (ws + x, ws + y)) {
                    Some((x, _)) if x >= z => break,
                    // Whole inside the window (or the file ends there).
                    Some((x, y)) if y + overlap <= we || we == n => return Some((x, y)),
                    // It may run past the window: look again from its start,
                    // unless that is where this window began already.
                    Some((x, y)) if x == s => return Some((x, y)),
                    Some((x, _)) => s = x,
                    None if we >= n => break,
                    None => s = (we - overlap).max(s + 1),
                }
                seen += win;
            }
        }
        None
    } else {
        // The last match starting in [0, from), then (wrapping) in [from, n).
        let mut ranges = vec![(0, from)];
        if limit.is_none() {
            ranges.push((from, n));
        }
        for (a, z) in ranges {
            let mut e = z;
            while e > a && seen < budget {
                let s = e.saturating_sub(win).max(a);
                // Room after `e` so a match starting before it is whole.
                let (ws, _, text) = window(doc, s, e + overlap);
                // Only trust matches clear of the window's start (a word
                // boundary there could be a false one), unless at `a`.
                let floor = if s == a { a } else { s + overlap };
                if let Some((x, y)) = m.find_before(&text, e - ws).map(|(x, y)| (ws + x, ws + y)) {
                    if x >= floor && x < e {
                        return Some((x, y));
                    }
                }
                if s == a {
                    break;
                }
                e = s + overlap;
                seen += win;
            }
        }
        None
    }
}

pub struct FifRequest {
    pub query: Query,
    pub dir: String,
    pub filter: String,
}

fn glob_match(pat: &str, name: &str) -> bool {
    let (p, n) = (pat.as_bytes(), name.as_bytes());
    let (mut pi, mut ni, mut star, mut mark) = (0, 0, usize::MAX, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi].eq_ignore_ascii_case(&n[ni])) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = pi;
            mark = ni;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Walks `dir` and returns Notepad++-style results text.
pub fn find_in_files(req: &FifRequest) -> String {
    let m = match req.query.compile() {
        Ok(m) => m,
        Err(e) => return format!("Search \"{}\": invalid pattern ({e})\n", req.query.pat),
    };
    let filters: Vec<&str> = req.filter.split([';', ' ', ',']).map(str::trim).filter(|s| !s.is_empty()).collect();
    let mut body = String::new();
    let (mut hits, mut files_hit, mut searched) = (0usize, 0usize, 0usize);
    let mut stack = vec![std::path::PathBuf::from(&req.dir)];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries.into_iter().rev() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !matches!(name.as_str(), ".git" | ".svn" | ".hg" | "node_modules" | "target" | ".vs") {
                    stack.push(p);
                }
                continue;
            }
            if !filters.is_empty() && !filters.iter().any(|f| glob_match(f, &name)) {
                continue;
            }
            if let Some(found) = search_file(&p, &m, &mut searched) {
                files_hit += 1;
                hits += found.0;
                body += &format!("  {} ({} hit{})\n", p.display(), found.0, if found.0 == 1 { "" } else { "s" });
                body += &found.1;
            }
            if hits > 50_000 {
                body += "  (stopped after 50000 hits)\n";
                stack.clear();
                break;
            }
        }
    }
    format!(
        "Search \"{}\" ({hits} hit{} in {files_hit} file{} of {searched} searched) in {}\n{body}",
        req.query.pat,
        if hits == 1 { "" } else { "s" },
        if files_hit == 1 { "" } else { "s" },
        req.dir
    )
}

/// Hits in one file: (count, result lines). None for no hit, or a file
/// that is too big or not text.
pub fn search_path(p: &Path, m: &Matcher) -> Option<(usize, String)> {
    let mut n = 0;
    search_file(p, m, &mut n)
}

fn search_file(p: &Path, m: &Matcher, searched: &mut usize) -> Option<(usize, String)> {
    let meta = std::fs::metadata(p).ok()?;
    if meta.len() > 64 << 20 {
        return None;
    }
    let raw = std::fs::read(p).ok()?;
    let head = &raw[..raw.len().min(8000)];
    let enc = crate::enc::detect(&raw);
    if head.contains(&0) && !matches!(enc, crate::enc::Enc::Utf16Le | crate::enc::Enc::Utf16Be) {
        return None;
    }
    *searched += 1;
    let d = crate::enc::decode(raw, None);
    let text = String::from_utf8_lossy(&d.text);
    let found = m.find_all(&text, 10_000);
    if found.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut last_line = usize::MAX;
    let mut line_no = 0;
    let mut line_start = 0;
    let mut scan = 0;
    for (a, _) in &found {
        while let Some(k) = text[scan..*a].find('\n') {
            line_no += 1;
            scan += k + 1;
            line_start = scan;
        }
        scan = *a;
        if line_no == last_line {
            continue;
        }
        last_line = line_no;
        let end = text[line_start..].find('\n').map(|k| line_start + k).unwrap_or(text.len());
        let mut line = text[line_start..end].trim().to_string();
        if line.len() > 400 {
            let mut cut = 400;
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            line.truncate(cut);
            line.push('…');
        }
        out += &format!("    Line {}: {}\n", line_no + 1, line);
    }
    Some((found.len(), out))
}

/// For a results-buffer line, returns (file, line number) to open.
pub fn result_target(lines: &[String], idx: usize) -> Option<(String, usize)> {
    let l = lines.get(idx)?.trim_start();
    let n: usize = l.strip_prefix("Line ")?.split(':').next()?.parse().ok()?;
    for k in (0..idx).rev() {
        let h = &lines[k];
        if h.starts_with("  ") && !h.starts_with("    ") {
            let h = h.trim();
            let path = h.rsplit_once(" (").map(|(p, _)| p).unwrap_or(h);
            return Some((path.to_string(), n));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windowed search finds exactly what a whole-text search finds.
    #[test]
    fn windowed_search_matches_whole_text_search() {
        let mut seed = 5u64;
        let mut rnd = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed as usize >> 3) % n
        };
        let words = ["SPOOL", "weld", "W042", "caf\u{e9}", "x", "\n", " ", "line"];
        for _ in 0..40 {
            let text: String = (0..3000).map(|_| words[rnd(words.len())]).collect();
            let mut doc = crate::doc::Doc::load(text.clone().into_bytes());
            for pat in ["weld", "W042 ", "caf\u{e9}x", "line\n"] {
                let m = Query { pat: pat.into(), ..Default::default() }.compile().unwrap();
                for _ in 0..30 {
                    let mut from = rnd(text.len() + 1);
                    while !text.is_char_boundary(from) {
                        from -= 1;
                    }
                    for forward in [true, false] {
                        let whole = if forward {
                            m.find_from(&text, from).or_else(|| m.find_from(&text, 0))
                        } else {
                            m.find_before(&text, from).or_else(|| m.find_before(&text, text.len() + 1))
                        };
                        let win = doc_find_with(&mut doc, &m, from, forward, None, 0, 256);
                        assert_eq!(win, whole, "pat {pat:?} from {from} forward {forward}");
                        // And straight in memory, with the gap somewhere inside.
                        if let Some(l) = &m.lit {
                            let mut d = crate::doc::Doc::load(text.clone().into_bytes());
                            let g = rnd(text.len() + 1);
                            let g = (0..=g).rev().find(|&k| text.is_char_boundary(k)).unwrap();
                            d.gap.place_gap(g);
                            assert_eq!(lit_find_doc(&d, l, from, forward, None), whole, "in memory: pat {pat:?} from {from} forward {forward} gap {g}");
                        }
                    }
                }
            }
        }
    }

    /// The fast literal search finds exactly what the regex engine finds.
    #[test]
    fn literal_search_matches_regex_search() {
        let mut seed = 99u64;
        let mut rnd = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed as usize >> 3) % n
        };
        let pieces = ["Weld", "weld", "WELD", "_", " ", "w", "eld", "aa", "a", "-", "\n", "caf\u{e9}", "9"];
        for _ in 0..300 {
            let text: String = (0..60).map(|_| pieces[rnd(pieces.len())]).collect();
            for pat in ["weld", "WeLd", "aa", "a", "-w", " w", "eld ", "caf\u{e9}"] {
                for (case, word) in [(false, false), (true, false), (false, true), (true, true)] {
                    let q = Query { pat: pat.into(), case, word, regex: false };
                    let fast = q.compile().unwrap();
                    let mut slow = q.compile().unwrap();
                    slow.lit = None;
                    assert_eq!(fast.find_all(&text, 1000), slow.find_all(&text, 1000), "{pat:?} case {case} word {word} in {text:?}");
                    for at in [0, text.len() / 3, text.len() / 2, text.len()] {
                        let at = (0..=at).rev().find(|&k| text.is_char_boundary(k)).unwrap();
                        assert_eq!(fast.find_from(&text, at), slow.find_from(&text, at), "{pat:?} from {at} in {text:?}");
                        assert_eq!(fast.find_before(&text, at), slow.find_before(&text, at), "{pat:?} before {at} in {text:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn literal_word_and_case() {
        let q = Query { pat: "a.b".into(), ..Default::default() };
        let m = q.compile().unwrap();
        assert_eq!(m.find_all("a.b axb A.B", 10), vec![(0, 3), (8, 11)]);
        let q = Query { pat: "cat".into(), word: true, case: true, ..Default::default() };
        assert_eq!(q.compile().unwrap().find_all("cat concat Cat cat", 10), vec![(0, 3), (15, 18)]);
    }

    #[test]
    fn regex_expand_and_prev() {
        let q = Query { pat: r"(\w+)=(\d+)".into(), regex: true, case: true, ..Default::default() };
        let m = q.compile().unwrap();
        let t = "x=1 y=22";
        let r = m.find_from(t, 1).unwrap();
        assert_eq!(r, (4, 8));
        assert_eq!(m.expand(&q, t, r, "$2:$1"), "22:y");
        assert_eq!(m.find_before(t, 4), Some((0, 3)));
    }

    #[test]
    fn globs_and_result_lines() {
        assert!(glob_match("*.RS", "main.rs"));
        assert!(!glob_match("*.rs", "main.rsx"));
        let lines: Vec<String> = ["Search ...", "  C:\\a b\\x.txt (2 hits)", "    Line 4: foo", "    Line 9: foo"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(result_target(&lines, 3), Some(("C:\\a b\\x.txt".into(), 9)));
    }
}
