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
        RegexBuilder::new(&p)
            .case_insensitive(!self.case)
            .multi_line(true)
            .build()
            .map(|re| Matcher { re })
            .map_err(|e| e.to_string().lines().last().unwrap_or("bad pattern").to_string())
    }
}

impl Matcher {
    pub fn find_all(&self, text: &str, limit: usize) -> Vec<(usize, usize)> {
        self.re.find_iter(text).filter(|m| m.end() > m.start()).take(limit).map(|m| (m.start(), m.end())).collect()
    }

    pub fn find_from(&self, text: &str, from: usize) -> Option<(usize, usize)> {
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
    let d = crate::enc::decode(&raw, None);
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
