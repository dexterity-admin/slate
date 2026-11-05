//! Revision compare: a line diff (patience alignment with an LCS fallback
//! for regions without unique lines), aligned into two equal-height sides
//! with filler lines, plus word-level marks inside changed lines.

use std::path::PathBuf;

/// Kind of an aligned line, the same index on both sides.
pub const SAME: u8 = 0;
/// Present on both sides with different text.
pub const CHANGED: u8 = 1;
/// Only on the left (removed); the right side shows a filler line.
pub const LEFT: u8 = 2;
/// Only on the right (added); the left side shows a filler line.
pub const RIGHT: u8 = 3;

/// Largest region (lines × lines) the quadratic LCS fallback handles; a
/// bigger region without unique lines is shown as one replaced block.
const LCS_CELLS: usize = 4_000_000;
/// Lines with more tokens than this get one prefix/suffix mark instead of
/// word marks.
const MAX_TOKENS: usize = 400;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Opts {
    pub ws: bool,
    pub case: bool,
}

/// Where a side's text came from, so the compare can be refreshed.
#[derive(Clone)]
pub enum Src {
    Buf(u32),
    File(PathBuf),
}

/// One side of a compare, attached to its read-only buffer.
pub struct DiffView {
    pub partner: u32,
    pub left: bool,
    pub src: Src,
    pub kinds: Vec<u8>,
    /// Original line number (1-based) per aligned line; 0 on filler lines.
    pub nums: Vec<u32>,
    /// Changed byte ranges inside lines: (aligned line, start, end), sorted.
    pub marks: Vec<(u32, u32, u32)>,
    /// Change blocks as (first line, end line exclusive).
    pub blocks: Vec<(u32, u32)>,
    pub stats: Stats,
    pub opts: Opts,
}

impl DiffView {
    pub fn is_filler(&self, line: usize) -> bool {
        let k = self.kinds.get(line).copied().unwrap_or(SAME);
        (self.left && k == RIGHT) || (!self.left && k == LEFT)
    }

    pub fn marks_on(&self, line: usize) -> &[(u32, u32, u32)] {
        let l = line as u32;
        let a = self.marks.partition_point(|m| m.0 < l);
        let z = a + self.marks[a..].partition_point(|m| m.0 == l);
        &self.marks[a..z]
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Stats {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
}

pub struct Side {
    pub text: Vec<u8>,
    pub nums: Vec<u32>,
    pub marks: Vec<(u32, u32, u32)>,
}

pub struct Aligned {
    pub left: Side,
    pub right: Side,
    pub kinds: Vec<u8>,
    pub blocks: Vec<(u32, u32)>,
    pub stats: Stats,
}

/// Splits text into lines (without their line endings); CRLF, LF and CR all
/// end a line, and the text after the last ending is a line too.
pub fn split_lines(t: &[u8]) -> Vec<&[u8]> {
    // Old Mac files end lines with a lone CR; everything else has LF.
    let eol = if find(t, b'\n').is_none() && find(t, b'\r').is_some() { b'\r' } else { b'\n' };
    let mut out = Vec::new();
    let mut s = 0;
    while let Some(k) = find(&t[s..], eol) {
        let l = &t[s..s + k];
        out.push(l.strip_suffix(b"\r").unwrap_or(l));
        s += k + 1;
    }
    out.push(&t[s..]);
    out
}

extern "C" {
    fn memchr(s: *const u8, c: i32, n: usize) -> *const u8;
}

/// First `c` in `t`, through the C runtime's vectorised memchr: the
/// size-optimised build scans bytes several times slower on its own.
pub fn find(t: &[u8], c: u8) -> Option<usize> {
    if t.is_empty() {
        return None;
    }
    let p = unsafe { memchr(t.as_ptr(), c as i32, t.len()) };
    (!p.is_null()).then(|| p as usize - t.as_ptr() as usize)
}

/// Hash of a line as compared: whitespace dropped and case folded when the
/// options say so. 64 bits make collisions negligible for this use.
fn line_hash(l: &[u8], o: Opts) -> u64 {
    const K: u64 = 0x9E3779B97F4A7C15;
    let mut h: u64 = l.len() as u64;
    if !o.ws && !o.case {
        // Eight bytes at a time: this runs over every byte of both files.
        let mut ch = l.chunks_exact(8);
        for c in &mut ch {
            h = (h ^ u64::from_le_bytes(c.try_into().unwrap())).wrapping_mul(K).rotate_left(29);
        }
        for &c in ch.remainder() {
            h = (h ^ c as u64).wrapping_mul(K).rotate_left(29);
        }
    } else {
        h = 0;
        for &c in l {
            if o.ws && matches!(c, b' ' | b'\t' | 0x0B | 0x0C) {
                continue;
            }
            let c = if o.case { c.to_ascii_lowercase() } else { c };
            h = (h ^ c as u64).wrapping_mul(K).rotate_left(29);
        }
    }
    // Finalise (splitmix64) so every bit depends on every byte; the hash
    // tables below use these values directly.
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58476D1CE4E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D049BB133111EB);
    h ^ (h >> 31)
}

/// Occurrence counts per line hash for the patience step: a plain
/// open-addressing table, several times faster than a generic hash map in
/// the size-optimised build. Counts saturate at 2 (only "once" matters).
struct Counts {
    keys: Vec<u64>,
    /// (count in a, count in b, position in b)
    vals: Vec<(u8, u8, u32)>,
    mask: usize,
}

impl Counts {
    fn new(n: usize) -> Self {
        let cap = (n * 2).next_power_of_two().max(16);
        Counts { keys: vec![0; cap], vals: vec![(0, 0, 0); cap], mask: cap - 1 }
    }

    /// Slot of `h`, or the empty slot where it would go. Hash 0 marks an
    /// empty slot, so it is stored as 1.
    fn slot(&self, h: u64) -> usize {
        let h = h.max(1);
        let mut k = h as usize & self.mask;
        while self.keys[k] != 0 && self.keys[k] != h {
            k = (k + 1) & self.mask;
        }
        k
    }

    fn add_a(&mut self, h: u64) {
        let k = self.slot(h);
        self.keys[k] = h.max(1);
        self.vals[k].0 = (self.vals[k].0 + 1).min(2);
    }

    fn add_b(&mut self, h: u64, j: u32) {
        let k = self.slot(h);
        if self.keys[k] != 0 {
            let v = &mut self.vals[k];
            v.1 = (v.1 + 1).min(2);
            v.2 = j;
        }
    }

    /// Position in b when `h` occurs exactly once on each side.
    fn unique(&self, h: u64) -> Option<u32> {
        let v = self.vals[self.slot(h)];
        (v.0 == 1 && v.1 == 1).then_some(v.2)
    }
}

/// Matched line pairs (index in a, index in b), ascending in both.
pub fn diff_lines(a: &[&[u8]], b: &[&[u8]], o: Opts) -> Vec<(usize, usize)> {
    let ha: Vec<u64> = a.iter().map(|l| line_hash(l, o)).collect();
    let hb: Vec<u64> = b.iter().map(|l| line_hash(l, o)).collect();
    let mut out = Vec::with_capacity(a.len().min(b.len()));
    // An explicit stack instead of recursion: deeply nested regions cannot
    // overflow it. Each entry is a region, or a match to emit in order.
    enum Job {
        Region(usize, usize, usize, usize),
        Match(usize, usize, usize),
    }
    let mut stack = vec![Job::Region(0, ha.len(), 0, hb.len())];
    while let Some(job) = stack.pop() {
        let (mut a0, a1, mut b0, b1) = match job {
            Job::Match(i, j, n) => {
                out.extend((0..n).map(|k| (i + k, j + k)));
                continue;
            }
            Job::Region(a0, a1, b0, b1) => (a0, a1, b0, b1),
        };
        while a0 < a1 && b0 < b1 && ha[a0] == hb[b0] {
            out.push((a0, b0));
            a0 += 1;
            b0 += 1;
        }
        let mut s = 0;
        while a1 - s > a0 && b1 - s > b0 && ha[a1 - s - 1] == hb[b1 - s - 1] {
            s += 1;
        }
        let (ae, be) = (a1 - s, b1 - s);
        // Jobs run last-in first-out, so push them in reverse order.
        stack.push(Job::Match(ae, be, s));
        if a0 == ae || b0 == be {
            continue;
        }
        let anchors = unique_anchors(&ha, &hb, a0, ae, b0, be);
        if anchors.is_empty() {
            let mut m = Vec::new();
            lcs(&ha[a0..ae], &hb[b0..be], &mut m);
            // The fallback's matches go out after earlier pushes have run;
            // they are complete, so emit them as single-line match jobs.
            for &(i, j) in m.iter().rev() {
                stack.push(Job::Match(a0 + i, b0 + j, 1));
            }
            continue;
        }
        let mut regions = Vec::new();
        let (mut pa, mut pb) = (a0, b0);
        let mut k = 0;
        while k < anchors.len() {
            // Consecutive anchors form one run with no gap regions between.
            let (i, j) = anchors[k];
            let mut n = 1;
            while k + n < anchors.len() && anchors[k + n] == (i + n, j + n) {
                n += 1;
            }
            regions.push(Job::Region(pa, i, pb, j));
            regions.push(Job::Match(i, j, n));
            pa = i + n;
            pb = j + n;
            k += n;
        }
        regions.push(Job::Region(pa, ae, pb, be));
        stack.extend(regions.into_iter().rev());
    }
    out
}

/// Lines that occur exactly once in both regions, longest increasing run
/// of them (patience diff).
fn unique_anchors(ha: &[u64], hb: &[u64], a0: usize, a1: usize, b0: usize, b1: usize) -> Vec<(usize, usize)> {
    let mut t = Counts::new(a1 - a0);
    for &h in &ha[a0..a1] {
        t.add_a(h);
    }
    for (j, &h) in hb[b0..b1].iter().enumerate() {
        t.add_b(h, (b0 + j) as u32);
    }
    let cands: Vec<(usize, usize)> = (a0..a1).filter_map(|i| t.unique(ha[i]).map(|j| (i, j as usize))).collect();
    if cands.is_empty() {
        return cands;
    }
    // Longest increasing subsequence on the b positions (patience sorting).
    let mut tails: Vec<usize> = Vec::new(); // index into cands
    let mut prev: Vec<usize> = vec![usize::MAX; cands.len()];
    for (k, &(_, j)) in cands.iter().enumerate() {
        // Mostly in order: appending is the common case.
        let p = match tails.last() {
            Some(&t) if cands[t].1 < j => tails.len(),
            _ => tails.partition_point(|&t| cands[t].1 < j),
        };
        if p > 0 {
            prev[k] = tails[p - 1];
        }
        if p == tails.len() {
            tails.push(k);
        } else {
            tails[p] = k;
        }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut k = *tails.last().unwrap();
    loop {
        out.push(cands[k]);
        if prev[k] == usize::MAX {
            break;
        }
        k = prev[k];
    }
    out.reverse();
    out
}

/// Classic LCS on a small region; appends matched (i, j) pairs.
fn lcs(a: &[u64], b: &[u64], out: &mut Vec<(usize, usize)>) {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 || n.saturating_mul(m) > LCS_CELLS {
        return;
    }
    // Lengths fit u16: with n*m <= 4M, min(n, m) <= 2000.
    let w = m + 1;
    let mut t = vec![0u16; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i * w + j] = if a[i] == b[j] { t[(i + 1) * w + j + 1] + 1 } else { t[(i + 1) * w + j].max(t[i * w + j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
}

/// Builds both aligned sides from two texts.
pub fn align(ta: &[u8], tb: &[u8], o: Opts) -> Aligned {
    let a = split_lines(ta);
    let b = split_lines(tb);
    let pairs = diff_lines(&a, &b, o);
    let cap = a.len().max(b.len()) + 16;
    let mut al = Side { text: Vec::with_capacity(ta.len() + cap), nums: Vec::with_capacity(cap), marks: Vec::new() };
    let mut ar = Side { text: Vec::with_capacity(tb.len() + cap), nums: Vec::with_capacity(cap), marks: Vec::new() };
    let mut kinds: Vec<u8> = Vec::with_capacity(cap);
    let mut stats = Stats::default();
    let push = |s: &mut Side, line: Option<(usize, &[u8])>| {
        if !s.nums.is_empty() {
            s.text.push(b'\n');
        }
        match line {
            Some((n, t)) => {
                s.text.extend_from_slice(t);
                s.nums.push(n as u32 + 1);
            }
            None => s.nums.push(0),
        }
    };
    let (mut i, mut j) = (0usize, 0usize);
    let ends = pairs.iter().copied().chain(std::iter::once((a.len(), b.len())));
    for (pi, pj) in ends {
        // Unmatched a[i..pi] and b[j..pj]: pair them up as changed lines,
        // the rest are removed / added.
        let (dl, il) = (pi - i, pj - j);
        let paired = dl.min(il);
        for k in 0..paired {
            let line = kinds.len() as u32;
            let (x, y) = (a[i + k], b[j + k]);
            push(&mut al, Some((i + k, x)));
            push(&mut ar, Some((j + k, y)));
            if line_hash(x, o) == line_hash(y, o) {
                // Equal under the options (whitespace / case only).
                kinds.push(SAME);
                continue;
            }
            kinds.push(CHANGED);
            stats.changed += 1;
            word_marks(x, y, o, line, &mut al.marks, &mut ar.marks);
        }
        for k in paired..dl {
            push(&mut al, Some((i + k, a[i + k])));
            push(&mut ar, None);
            kinds.push(LEFT);
            stats.removed += 1;
        }
        for k in paired..il {
            push(&mut al, None);
            push(&mut ar, Some((j + k, b[j + k])));
            kinds.push(RIGHT);
            stats.added += 1;
        }
        if pi < a.len() && pj < b.len() {
            push(&mut al, Some((pi, a[pi])));
            push(&mut ar, Some((pj, b[pj])));
            kinds.push(SAME);
        }
        i = pi + 1;
        j = pj + 1;
    }
    let mut blocks = Vec::new();
    let mut k = 0;
    while k < kinds.len() {
        if kinds[k] == SAME {
            k += 1;
            continue;
        }
        let s = k;
        while k < kinds.len() && kinds[k] != SAME {
            k += 1;
        }
        blocks.push((s as u32, k as u32));
    }
    Aligned { left: al, right: ar, kinds, blocks, stats }
}

#[derive(Clone, Copy, PartialEq)]
enum TokKind {
    Word,
    Space,
    Other,
}

fn tok_kind(c: u8) -> TokKind {
    if c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 {
        TokKind::Word
    } else if c == b' ' || c == b'\t' {
        TokKind::Space
    } else {
        TokKind::Other
    }
}

/// Token spans: runs of word characters, runs of blanks, single others.
fn tokens(l: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < l.len() {
        let k = tok_kind(l[i]);
        let s = i;
        i += 1;
        if k != TokKind::Other {
            while i < l.len() && tok_kind(l[i]) == k {
                i += 1;
            }
        }
        out.push((s, i));
    }
    out
}

fn tok_eq(x: &[u8], y: &[u8], o: Opts) -> bool {
    if o.ws && tok_kind(x[0]) == TokKind::Space && tok_kind(y[0]) == TokKind::Space {
        return true;
    }
    if o.case {
        x.eq_ignore_ascii_case(y)
    } else {
        x == y
    }
}

/// Marks the differing parts of a changed line pair.
fn word_marks(x: &[u8], y: &[u8], o: Opts, line: u32, mx: &mut Vec<(u32, u32, u32)>, my: &mut Vec<(u32, u32, u32)>) {
    let (tx, ty) = (tokens(x), tokens(y));
    let space = |l: &[u8], (s, e): (usize, usize)| o.ws && tok_kind(l[s]) == TokKind::Space && e > s;
    if tx.len() > MAX_TOKENS || ty.len() > MAX_TOKENS {
        // One mark between the common prefix and suffix.
        let p = x.iter().zip(y).take_while(|(a, b)| a == b).count();
        let s = x[p..].iter().rev().zip(y[p..].iter().rev()).take_while(|(a, b)| a == b).count();
        if x.len() - s > p {
            mx.push((line, p as u32, (x.len() - s) as u32));
        }
        if y.len() - s > p {
            my.push((line, p as u32, (y.len() - s) as u32));
        }
        return;
    }
    let (n, m) = (tx.len(), ty.len());
    let w = m + 1;
    let mut t = vec![0u16; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i * w + j] = if tok_eq(&x[tx[i].0..tx[i].1], &y[ty[j].0..ty[j].1], o) {
                t[(i + 1) * w + j + 1] + 1
            } else {
                t[(i + 1) * w + j].max(t[i * w + j + 1])
            };
        }
    }
    let mut keep_x = vec![false; n];
    let mut keep_y = vec![false; m];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if tok_eq(&x[tx[i].0..tx[i].1], &y[ty[j].0..ty[j].1], o) {
            keep_x[i] = true;
            keep_y[j] = true;
            i += 1;
            j += 1;
        } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let emit = |l: &[u8], toks: &[(usize, usize)], keep: &[bool], out: &mut Vec<(u32, u32, u32)>| {
        let mut k = 0;
        while k < toks.len() {
            if keep[k] || space(l, toks[k]) {
                k += 1;
                continue;
            }
            let s = toks[k].0;
            let mut e = toks[k].1;
            k += 1;
            // Merge runs of changed tokens, across single blanks between them.
            while k < toks.len() {
                if !keep[k] {
                    e = toks[k].1;
                    k += 1;
                } else if tok_kind(l[toks[k].0]) == TokKind::Space && k + 1 < toks.len() && !keep[k + 1] {
                    e = toks[k + 1].1;
                    k += 2;
                } else {
                    break;
                }
            }
            out.push((line, s as u32, e as u32));
        }
    };
    emit(x, &tx, &keep_x, mx);
    emit(y, &ty, &keep_y, my);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(a: &str, b: &str, o: Opts) -> String {
        align(a.as_bytes(), b.as_bytes(), o).kinds.iter().map(|k| ['=', '~', '-', '+'][*k as usize]).collect()
    }

    #[test]
    fn identical_and_empty() {
        assert_eq!(kinds("a\nb\nc", "a\nb\nc", Opts::default()), "===");
        assert_eq!(kinds("", "", Opts::default()), "=");
        let al = align(b"", b"x\ny", Opts::default());
        assert_eq!(al.stats, Stats { added: 1, removed: 0, changed: 1 });
    }

    #[test]
    fn insert_delete_change() {
        let a = "PIPE\n  END-POINT 0 0 0\n  SKEY BEBW\nELBOW\n  ANGLE 90\nFLANGE";
        let b = "PIPE\n  END-POINT 0 0 0\n  SKEY BEBW\nTEE\nELBOW\n  ANGLE 45\nFLANGE";
        assert_eq!(kinds(a, b, Opts::default()), "===+=~=");
        let al = align(a.as_bytes(), b.as_bytes(), Opts::default());
        // The right side gets the TEE, the left side a filler at its place.
        assert_eq!(al.left.nums, vec![1, 2, 3, 0, 4, 5, 6]);
        assert_eq!(al.right.nums, vec![1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(al.left.text, b"PIPE\n  END-POINT 0 0 0\n  SKEY BEBW\n\nELBOW\n  ANGLE 90\nFLANGE");
        // Only the angle value is marked inside the changed line.
        assert_eq!(al.left.marks, vec![(5, 8, 10)]);
        assert_eq!(al.right.marks, vec![(5, 8, 10)]);
        assert_eq!(al.blocks, vec![(3, 4), (5, 6)]);
    }

    #[test]
    fn options_ignore_whitespace_and_case() {
        let (a, b) = ("x = 1\nFoo", "x=1  \nfoo");
        assert_eq!(kinds(a, b, Opts::default()), "~~");
        assert_eq!(kinds(a, b, Opts { ws: true, case: false }), "=~");
        assert_eq!(kinds(a, b, Opts { ws: true, case: true }), "==");
    }

    #[test]
    fn repeated_lines_use_fallback() {
        // No unique lines at all: the LCS fallback still aligns them.
        let a = "x\ny\nx\ny\nx";
        let b = "x\ny\ny\nx";
        let al = align(a.as_bytes(), b.as_bytes(), Opts::default());
        assert_eq!(al.stats.removed + al.stats.changed, 1);
        assert_eq!(al.kinds.len(), 5);
    }

    #[test]
    fn line_endings_split() {
        assert_eq!(split_lines(b"a\r\nb\nc\r\n"), vec![&b"a"[..], b"b", b"c", b""]);
        assert_eq!(split_lines(b"a\rb\rc"), vec![&b"a"[..], b"b", b"c"]);
        assert_eq!(kinds("a\r\nb", "a\nb", Opts::default()), "==");
    }

    #[test]
    fn moved_block_alignment() {
        // A unique line moved far away: patience keeps the longer run.
        let mut a: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let mut b = a.clone();
        let moved = b.remove(10);
        b.push(moved);
        a.push("tail".into());
        b.push("tail".into());
        let al = align(a.join("\n").as_bytes(), b.join("\n").as_bytes(), Opts::default());
        assert_eq!(al.stats, Stats { added: 1, removed: 1, changed: 0 });
    }

    #[test]
    #[ignore]
    fn huge_compare_timing() {
        let n = 1_000_000;
        let a: String = (0..n).map(|i| format!("{i},LINE-{},{}\n", i % 977, i * 7)).collect();
        let b: String = (0..n).map(|i| if i % 1000 == 0 { format!("{i},CHANGED,{}\n", i) } else { format!("{i},LINE-{},{}\n", i % 977, i * 7) }).collect();
        let t = std::time::Instant::now();
        let al = align(a.as_bytes(), b.as_bytes(), Opts::default());
        eprintln!("1M lines: {:?}, {:?}", t.elapsed(), al.stats);
    }
}
