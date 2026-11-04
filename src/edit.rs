//! Text commands and caret movement on a `Buffer`, independent of the UI.

use crate::buffer::{Buffer, EditKind, Sel};
use crate::syntax::LANGS;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Move {
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    Up(usize),
    Down(usize),
    DocStart,
    DocEnd,
}

fn class(c: u8) -> u8 {
    if c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 {
        2
    } else if c == b' ' || c == b'\t' {
        0
    } else if c == b'\n' {
        3
    } else {
        1
    }
}

fn word_left(b: &Buffer, p: usize) -> usize {
    let mut q = p;
    while q > 0 && class(b.doc.byte(q - 1)) == 0 {
        q -= 1;
    }
    if q > 0 && class(b.doc.byte(q - 1)) == 3 {
        return if q == p { q - 1 } else { q };
    }
    if q > 0 {
        let k = class(b.doc.byte(q - 1));
        while q > 0 && class(b.doc.byte(q - 1)) == k {
            q -= 1;
        }
    }
    q
}

fn word_right(b: &Buffer, p: usize) -> usize {
    let n = b.doc.len();
    let mut q = p;
    if q < n && class(b.doc.byte(q)) == 3 {
        return q + 1;
    }
    if q < n {
        let k = class(b.doc.byte(q));
        while q < n && class(b.doc.byte(q)) == k && k != 3 {
            q += 1;
        }
    }
    while q < n && class(b.doc.byte(q)) == 0 {
        q += 1;
    }
    q
}

/// Moves every caret; with `extend` the anchors stay put.
pub fn move_carets(b: &mut Buffer, how: Move, extend: bool) {
    let vertical = matches!(how, Move::Up(_) | Move::Down(_));
    let keep_want = vertical && b.want_col.is_some();
    let sels = b.sels.clone();
    let mut want = b.want_col;
    let mut out = Vec::with_capacity(sels.len());
    for s in sels {
        if !extend && !s.is_empty() && matches!(how, Move::Left | Move::Right) {
            let p = if how == Move::Left { s.lo() } else { s.hi() };
            out.push(Sel::caret(p));
            continue;
        }
        let p = s.head;
        let line = b.doc.line_of(p);
        let np = match how {
            Move::Left => {
                if p > 0 {
                    let q = b.doc.prev_char(p);
                    let ql = b.doc.line_of(q);
                    match b.hidden_by(ql) {
                        Some(h) => b.doc.line_end(h),
                        None => q,
                    }
                } else {
                    0
                }
            }
            Move::Right => {
                if p == b.doc.line_end(line) {
                    match b.next_visible(line) {
                        Some(l) => b.doc.line_start(l),
                        None => p,
                    }
                } else {
                    b.doc.next_char(p)
                }
            }
            Move::WordLeft => word_left(b, p),
            Move::WordRight => word_right(b, p),
            Move::Home => {
                let s0 = b.doc.line_start(line);
                let first = s0 + b.line_indent(line).0;
                if p == first {
                    s0
                } else {
                    first
                }
            }
            Move::End => b.doc.line_end(line),
            Move::Up(n) | Move::Down(n) => {
                let col = want.unwrap_or_else(|| b.display_col(p));
                want = Some(col);
                let mut l = line;
                for _ in 0..n {
                    let next = if matches!(how, Move::Up(_)) { b.prev_visible(l) } else { b.next_visible(l) };
                    match next {
                        Some(x) => l = x,
                        None => break,
                    }
                }
                if l == line {
                    if matches!(how, Move::Up(_)) {
                        b.doc.line_start(line)
                    } else {
                        b.doc.line_end(line)
                    }
                } else {
                    b.pos_at_col(l, col)
                }
            }
            Move::DocStart => 0,
            Move::DocEnd => b.doc.len(),
        };
        out.push(if extend { Sel { anchor: s.anchor, head: np } } else { Sel::caret(np) });
    }
    b.sels = out;
    b.normalize();
    b.seal = true;
    b.want_col = if vertical { want } else { None };
    if !keep_want && !vertical {
        b.want_col = None;
    }
}

/// Inserts `text` at every caret, replacing selections.
pub fn insert(b: &mut Buffer, text: &[u8], kind: EditKind) {
    let ch = b.sorted_sels().iter().map(|s| (s.lo(), s.hi(), text.to_vec())).collect();
    b.apply(ch, kind);
}

/// Inserts one line of `parts` per caret when the counts match (column paste).
pub fn insert_each(b: &mut Buffer, parts: &[Vec<u8>]) {
    let sels = b.sorted_sels();
    let ch = sels.iter().zip(parts).map(|(s, t)| (s.lo(), s.hi(), t.clone())).collect();
    b.apply(ch, EditKind::Other);
}

pub fn newline(b: &mut Buffer) {
    let sels = b.sorted_sels();
    let mut ch = Vec::new();
    for s in sels {
        let line = b.doc.line_of(s.lo());
        let ls = b.doc.line_start(line);
        let ind = b.line_indent(line).0.min(s.lo() - ls);
        let mut t = vec![b'\n'];
        t.extend(b.doc.text(ls, ls + ind));
        // Open-bracket lines indent one level deeper.
        if s.lo() > ls && matches!(b.doc.byte(s.lo() - 1), b'{' | b'[' | b'(' | b':') && LANGS[b.lang].kind != crate::syntax::Kind::Plain {
            t.extend(indent_unit(b));
        }
        ch.push((s.lo(), s.hi(), t));
    }
    b.apply(ch, EditKind::Other);
}

pub fn indent_unit(b: &Buffer) -> Vec<u8> {
    if b.use_tabs() {
        b"\t".to_vec()
    } else {
        vec![b' '; b.tab_size]
    }
}

impl Buffer {
    pub fn use_tabs(&self) -> bool {
        crate::settings::get().use_tabs
    }
}

pub fn backspace(b: &mut Buffer) {
    let sels = b.sorted_sels();
    let mut ch = Vec::new();
    for s in sels {
        if !s.is_empty() {
            ch.push((s.lo(), s.hi(), Vec::new()));
        } else if s.head > 0 {
            let p = s.head;
            let line = b.doc.line_of(p);
            let ls = b.doc.line_start(line);
            let (ind, _) = b.line_indent(line);
            // In leading spaces, delete back to the previous tab stop.
            if !b.use_tabs() && p > ls && p <= ls + ind && b.doc.byte(p - 1) == b' ' {
                let col = b.display_col(p);
                let target = (col - 1) / b.tab_size * b.tab_size;
                let mut q = p;
                while q > ls && b.doc.byte(q - 1) == b' ' && b.display_col(q) > target {
                    q -= 1;
                }
                ch.push((q, p, Vec::new()));
            } else {
                let q = b.doc.prev_char(p);
                let ql = b.doc.line_of(q);
                let q = match b.hidden_by(ql) {
                    Some(h) => b.doc.line_end(h),
                    None => q,
                };
                ch.push((q, p, Vec::new()));
            }
        }
    }
    dedupe(&mut ch);
    b.apply(ch, EditKind::Typing);
}

pub fn delete(b: &mut Buffer) {
    let sels = b.sorted_sels();
    let n = b.doc.len();
    let mut ch = Vec::new();
    for s in sels {
        if !s.is_empty() {
            ch.push((s.lo(), s.hi(), Vec::new()));
        } else if s.head < n {
            ch.push((s.head, b.doc.next_char(s.head), Vec::new()));
        }
    }
    dedupe(&mut ch);
    b.apply(ch, EditKind::Typing);
}

pub fn delete_word(b: &mut Buffer, forward: bool) {
    let sels = b.sorted_sels();
    let mut ch = Vec::new();
    for s in sels {
        if !s.is_empty() {
            ch.push((s.lo(), s.hi(), Vec::new()));
        } else if forward {
            ch.push((s.head, word_right(b, s.head), Vec::new()));
        } else {
            ch.push((word_left(b, s.head), s.head, Vec::new()));
        }
    }
    dedupe(&mut ch);
    b.apply(ch, EditKind::Other);
}

fn dedupe(ch: &mut Vec<(usize, usize, Vec<u8>)>) {
    ch.sort_by_key(|c| c.0);
    let mut out: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    for c in ch.drain(..) {
        if let Some(l) = out.last_mut() {
            if c.0 < l.1 {
                l.1 = l.1.max(c.1);
                continue;
            }
        }
        out.push(c);
    }
    *ch = out;
}

/// Distinct line spans covered by the selections, merged.
pub fn sel_line_blocks(b: &Buffer) -> Vec<(usize, usize)> {
    let mut v: Vec<(usize, usize)> = b
        .sorted_sels()
        .iter()
        .map(|s| {
            let l0 = b.doc.line_of(s.lo());
            let mut l1 = b.doc.line_of(s.hi());
            if l1 > l0 && s.hi() == b.doc.line_start(l1) {
                l1 -= 1;
            }
            let l1 = b.folds.iter().filter(|f| f.0 == l1).map(|f| f.1).max().unwrap_or(l1);
            (l0, l1)
        })
        .collect();
    v.sort();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (a, z) in v {
        if let Some(l) = out.last_mut() {
            if a <= l.1 + 1 {
                l.1 = l.1.max(z);
                continue;
            }
        }
        out.push((a, z));
    }
    out
}

pub fn indent(b: &mut Buffer, outdent: bool) {
    let multi_line = b.sels.iter().any(|s| b.doc.line_of(s.lo()) != b.doc.line_of(s.hi()));
    if !outdent && !multi_line {
        let unit = if b.use_tabs() {
            b"\t".to_vec()
        } else {
            let col = b.display_col(b.caret());
            vec![b' '; b.tab_size - col % b.tab_size]
        };
        insert(b, &unit, EditKind::Typing);
        return;
    }
    let mut ch = Vec::new();
    for (l0, l1) in sel_line_blocks(b) {
        for l in l0..=l1 {
            let s = b.doc.line_start(l);
            if outdent {
                let mut k = 0;
                let mut w = 0;
                while s + k < b.doc.line_end(l) && w < b.tab_size {
                    match b.doc.byte(s + k) {
                        b' ' => w += 1,
                        b'\t' => w = b.tab_size,
                        _ => break,
                    }
                    k += 1;
                }
                if k > 0 {
                    ch.push((s, s + k, Vec::new()));
                }
            } else if b.doc.line_end(l) > s {
                ch.push((s, s, indent_unit(b)));
            }
        }
    }
    b.apply(ch, EditKind::Other);
}

pub fn toggle_comment(b: &mut Buffer) {
    let lang = &LANGS[b.lang];
    let blocks = sel_line_blocks(b);
    let Some(prefix) = lang.line.first() else {
        if let Some((open, close)) = lang.block {
            let mut ch = Vec::new();
            for (l0, l1) in blocks {
                let s = b.doc.line_start(l0) + b.line_indent(l0).0;
                let e = b.doc.line_end(l1);
                let t = b.doc.text(s, e);
                if t.starts_with(open.as_bytes()) && t.ends_with(close.as_bytes()) {
                    let inner = &t[open.len()..t.len() - close.len()];
                    let inner = inner.strip_prefix(b" ").unwrap_or(inner);
                    let inner = inner.strip_suffix(b" ").unwrap_or(inner);
                    ch.push((s, e, inner.to_vec()));
                } else {
                    let mut v = format!("{open} ").into_bytes();
                    v.extend_from_slice(&t);
                    v.extend_from_slice(format!(" {close}").as_bytes());
                    ch.push((s, e, v));
                }
            }
            b.apply(ch, EditKind::Other);
        }
        return;
    };
    let prefix = prefix.trim_end();
    let p = prefix.as_bytes();
    let mut all_commented = true;
    let mut min_ind = usize::MAX;
    for &(l0, l1) in &blocks {
        for l in l0..=l1 {
            let (k, w) = b.line_indent(l);
            if w.is_none() {
                continue;
            }
            min_ind = min_ind.min(k);
            let s = b.doc.line_start(l) + k;
            let t = b.doc.text(s, (s + p.len()).min(b.doc.line_end(l)));
            if !t.eq_ignore_ascii_case(p) {
                all_commented = false;
            }
        }
    }
    if min_ind == usize::MAX {
        return;
    }
    let mut ch = Vec::new();
    for (l0, l1) in blocks {
        for l in l0..=l1 {
            let (k, w) = b.line_indent(l);
            if w.is_none() {
                continue;
            }
            let ls = b.doc.line_start(l);
            if all_commented {
                let s = ls + k;
                let mut e = s + p.len();
                if e < b.doc.line_end(l) && b.doc.byte(e) == b' ' {
                    e += 1;
                }
                ch.push((s, e, Vec::new()));
            } else {
                let mut t = p.to_vec();
                t.push(b' ');
                ch.push((ls + min_ind, ls + min_ind, t));
            }
        }
    }
    b.apply(ch, EditKind::Other);
}

pub fn duplicate(b: &mut Buffer) {
    if b.sels.iter().all(|s| s.is_empty()) {
        let mut ch = Vec::new();
        for (l0, l1) in sel_line_blocks(b) {
            let s = b.doc.line_start(l0);
            let e = b.doc.line_end(l1);
            let mut t = b.doc.text(s, e);
            t.insert(0, b'\n');
            ch.push((e, e, t));
        }
        let before: Vec<Sel> = b.sels.clone();
        b.apply(ch, EditKind::Other);
        b.sels = before;
        b.normalize();
    } else {
        let ch = b.sorted_sels().iter().map(|s| (s.hi(), s.hi(), b.doc.text(s.lo(), s.hi()))).collect();
        let before = b.sels.clone();
        b.apply(ch, EditKind::Other);
        b.sels = before;
    }
}

pub fn delete_lines(b: &mut Buffer) {
    let mut ch = Vec::new();
    let n = b.doc.line_count();
    for (l0, l1) in sel_line_blocks(b) {
        let s = b.doc.line_start(l0);
        if l1 + 1 < n {
            ch.push((s, b.doc.line_start(l1 + 1), Vec::new()));
        } else if l0 > 0 {
            ch.push((b.doc.line_end(l0 - 1), b.doc.len(), Vec::new()));
        } else {
            ch.push((0, b.doc.len(), Vec::new()));
        }
    }
    b.apply(ch, EditKind::Other);
}

pub fn move_lines(b: &mut Buffer, up: bool) {
    let blocks = sel_line_blocks(b);
    let n = b.doc.line_count();
    if (up && blocks[0].0 == 0) || (!up && blocks.last().unwrap().1 + 1 >= n) {
        return;
    }
    let mut ch = Vec::new();
    let mut shift: isize = 0;
    for (l0, l1) in blocks {
        let block = b.doc.text(b.doc.line_start(l0), b.doc.line_end(l1));
        if up {
            let o = l0 - 1;
            let other = b.doc.text(b.doc.line_start(o), b.doc.line_end(o));
            let mut t = block.clone();
            t.push(b'\n');
            t.extend_from_slice(&other);
            ch.push((b.doc.line_start(o), b.doc.line_end(l1), t));
            shift = -((other.len() + 1) as isize);
        } else {
            let o = l1 + 1;
            let other = b.doc.text(b.doc.line_start(o), b.doc.line_end(o));
            let mut t = other.clone();
            t.push(b'\n');
            t.extend_from_slice(&block);
            ch.push((b.doc.line_start(l0), b.doc.line_end(o), t));
            shift = (other.len() + 1) as isize;
        }
    }
    let before = b.sels.clone();
    b.apply(ch, EditKind::Other);
    b.sels = before
        .iter()
        .map(|s| Sel { anchor: (s.anchor as isize + shift) as usize, head: (s.head as isize + shift) as usize })
        .collect();
    b.normalize();
}

/// Applies `f` to each selection (or the word at each caret).
pub fn transform_sel(b: &mut Buffer, f: impl Fn(&str) -> String) {
    let mut ch = Vec::new();
    let mut new_sels = Vec::new();
    let mut delta: isize = 0;
    for s in b.sorted_sels() {
        let (lo, hi) = if s.is_empty() { b.word_at(s.head) } else { (s.lo(), s.hi()) };
        let t = String::from_utf8_lossy(&b.doc.text(lo, hi)).into_owned();
        let r = f(&t).into_bytes();
        let nlo = (lo as isize + delta) as usize;
        new_sels.push(Sel { anchor: nlo, head: nlo + r.len() });
        delta += r.len() as isize - (hi - lo) as isize;
        ch.push((lo, hi, r));
    }
    b.apply(ch, EditKind::Other);
    b.sels = new_sels;
    b.normalize();
}

/// Replaces whole-line blocks (whole document if nothing is selected) by `f(lines)`.
pub fn transform_lines(b: &mut Buffer, f: impl Fn(Vec<String>) -> Vec<String>) {
    let whole = b.sels.iter().all(|s| s.is_empty());
    let blocks = if whole { vec![(0, b.doc.line_count() - 1)] } else { sel_line_blocks(b) };
    let mut ch = Vec::new();
    for (l0, l1) in blocks {
        let s = b.doc.line_start(l0);
        let e = b.doc.line_end(l1);
        let t = String::from_utf8_lossy(&b.doc.text(s, e)).into_owned();
        let out = f(t.split('\n').map(str::to_owned).collect()).join("\n");
        ch.push((s, e, out.into_bytes()));
    }
    b.apply(ch, EditKind::Other);
}

pub fn join_lines(b: &mut Buffer) {
    let mut ch = Vec::new();
    for (l0, l1) in sel_line_blocks(b) {
        let l1 = if l1 == l0 { (l0 + 1).min(b.doc.line_count() - 1) } else { l1 };
        for l in l0..l1 {
            let e = b.doc.line_end(l);
            let ns = b.doc.line_start(l + 1);
            let k = b.line_indent(l + 1).0;
            ch.push((e, ns + k, b" ".to_vec()));
        }
    }
    b.apply(ch, EditKind::Other);
}

pub fn select_all(b: &mut Buffer) {
    b.sels = vec![Sel { anchor: 0, head: b.doc.len() }];
}

pub fn select_line(b: &mut Buffer) {
    let blocks = sel_line_blocks(b);
    let n = b.doc.line_count();
    b.sels = blocks
        .iter()
        .map(|&(l0, l1)| Sel {
            anchor: b.doc.line_start(l0),
            head: if l1 + 1 < n { b.doc.line_start(l1 + 1) } else { b.doc.len() },
        })
        .collect();
    b.normalize();
}

/// Text of all selections joined by newlines (for copy).
pub fn selected_text(b: &Buffer) -> Vec<u8> {
    let sels = b.sorted_sels();
    let mut out = Vec::new();
    for (i, s) in sels.iter().enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        out.extend(b.doc.text(s.lo(), s.hi()));
    }
    out
}

/// Rectangular selection from `a` to `h` (display columns), one sel per line.
pub fn column_select(b: &mut Buffer, a: usize, h: usize) {
    let (al, hl) = (b.doc.line_of(a), b.doc.line_of(h));
    let (ac, hc) = (b.display_col(a), b.display_col(h));
    column_select_cols(b, al, ac, hl, hc);
}

pub fn column_select_cols(b: &mut Buffer, al: usize, ac: usize, hl: usize, hc: usize) {
    let (l0, l1) = (al.min(hl), al.max(hl));
    let mut v = Vec::new();
    for l in l0..=l1 {
        if b.hidden_by(l).is_some() {
            continue;
        }
        v.push(Sel { anchor: b.pos_at_col(l, ac), head: b.pos_at_col(l, hc) });
    }
    if hl < al {
        v.reverse();
    }
    b.sels = v;
    b.normalize();
}

/// Adds a selection on the next occurrence of the primary selection (or word).
pub fn add_next_occurrence(b: &mut Buffer) {
    let p = b.primary();
    if p.is_empty() {
        let (lo, hi) = b.word_at(p.head);
        if lo < hi {
            let n = b.sels.len();
            b.sels[n - 1] = Sel { anchor: lo, head: hi };
        }
        return;
    }
    let needle = b.doc.text(p.lo(), p.hi());
    let hay = b.text_all();
    let start = b.sels.iter().map(|s| s.hi()).max().unwrap_or(0);
    let find = |from: usize| hay[from..].windows(needle.len()).position(|w| w == &needle[..]).map(|k| k + from);
    let hit = find(start).or_else(|| find(0));
    if let Some(k) = hit {
        if !b.sels.iter().any(|s| s.lo() == k) {
            b.sels.push(Sel { anchor: k, head: k + needle.len() });
            b.normalize();
        }
    }
}

pub fn trim_trailing(b: &mut Buffer) {
    let mut ch = Vec::new();
    for l in 0..b.doc.line_count() {
        let (s, e) = (b.doc.line_start(l), b.doc.line_end(l));
        let mut k = e;
        while k > s && matches!(b.doc.byte(k - 1), b' ' | b'\t') {
            k -= 1;
        }
        if k < e {
            ch.push((k, e, Vec::new()));
        }
    }
    b.apply(ch, EditKind::Other);
}

pub fn tabs_spaces(b: &mut Buffer, to_spaces: bool) {
    let mut ch = Vec::new();
    for l in 0..b.doc.line_count() {
        let (k, w) = b.line_indent(l);
        let Some(w) = w else { continue };
        let s = b.doc.line_start(l);
        let new = if to_spaces {
            vec![b' '; w]
        } else {
            let mut v = vec![b'\t'; w / b.tab_size];
            v.extend(std::iter::repeat_n(b' ', w % b.tab_size));
            v
        };
        if b.doc.text(s, s + k) != new {
            ch.push((s, s + k, new));
        }
    }
    b.apply(ch, EditKind::Other);
}

/// Position of the bracket matching the one at or before `p`.
pub fn match_bracket(b: &Buffer, p: usize) -> Option<(usize, usize)> {
    let n = b.doc.len();
    let at = |q: usize| if q < n { Some(b.doc.byte(q)) } else { None };
    let pick = [p, p.wrapping_sub(1)].into_iter().find(|&q| q < n && matches!(at(q), Some(b'(' | b')' | b'[' | b']' | b'{' | b'}')))?;
    let c = b.doc.byte(pick);
    let (open, close, fwd) = match c {
        b'(' => (b'(', b')', true),
        b'[' => (b'[', b']', true),
        b'{' => (b'{', b'}', true),
        b')' => (b'(', b')', false),
        b']' => (b'[', b']', false),
        _ => (b'{', b'}', false),
    };
    let mut depth = 0i32;
    let limit = 1 << 20;
    if fwd {
        for q in pick..n.min(pick + limit) {
            match b.doc.byte(q) {
                x if x == open => depth += 1,
                x if x == close => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((pick, q));
                    }
                }
                _ => {}
            }
        }
    } else {
        let mut q = pick + 1;
        while q > 0 && pick + 1 - q < limit {
            q -= 1;
            match b.doc.byte(q) {
                x if x == close => depth += 1,
                x if x == open => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((pick, q));
                    }
                }
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> Buffer {
        Buffer::new(1, s.as_bytes().to_vec())
    }
    fn text(b: &Buffer) -> String {
        String::from_utf8(b.text_all()).unwrap()
    }

    #[test]
    fn backspace_joins_lines_and_deletes_utf8() {
        let mut b = buf("a€\nb");
        b.set_caret(5);
        backspace(&mut b);
        assert_eq!(text(&b), "a€b");
        backspace(&mut b);
        assert_eq!(text(&b), "ab");
        assert_eq!(b.caret(), 1);
    }

    #[test]
    fn newline_keeps_indent() {
        let mut b = buf("    x");
        b.set_caret(5);
        newline(&mut b);
        assert_eq!(text(&b), "    x\n    ");
    }

    #[test]
    fn comment_toggle_round_trip() {
        let mut b = buf("a\n  b\n");
        b.sels = vec![Sel { anchor: 0, head: 5 }];
        b.lang = crate::syntax::lang_for_path("x.py");
        toggle_comment(&mut b);
        assert_eq!(text(&b), "# a\n#   b\n");
        toggle_comment(&mut b);
        assert_eq!(text(&b), "a\n  b\n");
    }

    #[test]
    fn move_and_duplicate_lines() {
        let mut b = buf("1\n2\n3");
        b.set_caret(2);
        move_lines(&mut b, true);
        assert_eq!(text(&b), "2\n1\n3");
        assert_eq!(b.caret(), 0);
        duplicate(&mut b);
        assert_eq!(text(&b), "2\n2\n1\n3");
        delete_lines(&mut b);
        assert_eq!(text(&b), "2\n1\n3");
    }

    #[test]
    fn word_moves() {
        let mut b = buf("foo.bar  baz");
        b.set_caret(0);
        move_carets(&mut b, Move::WordRight, false);
        assert_eq!(b.caret(), 3);
        move_carets(&mut b, Move::WordRight, false);
        assert_eq!(b.caret(), 4);
        move_carets(&mut b, Move::WordRight, false);
        assert_eq!(b.caret(), 9);
        move_carets(&mut b, Move::WordLeft, true);
        assert_eq!(b.primary(), Sel { anchor: 9, head: 4 });
    }

    #[test]
    fn column_selection_and_typing() {
        let mut b = buf("abcd\nef\nghij");
        column_select_cols(&mut b, 0, 1, 2, 3);
        assert_eq!(b.sels.len(), 3);
        insert(&mut b, b"X", EditKind::Typing);
        assert_eq!(text(&b), "aXd\neX\ngXj");
    }

    #[test]
    fn next_occurrence_and_brackets() {
        let mut b = buf("ab x ab y ab");
        b.set_caret(0);
        add_next_occurrence(&mut b);
        add_next_occurrence(&mut b);
        add_next_occurrence(&mut b);
        assert_eq!(b.sels.len(), 3);
        let b2 = buf("f(a[1]{x})");
        assert_eq!(match_bracket(&b2, 1), Some((1, 9)));
        assert_eq!(match_bracket(&b2, 6), Some((6, 8)));
        assert_eq!(match_bracket(&b2, 10), Some((9, 1)));
    }
}
