//! One open document: text, selections, undo history and per-line
//! annotations (bookmarks, folds, highlight state).

use crate::doc::Doc;
use crate::ot::Op;
use crate::enc::{Enc, Eol};
use crate::syntax::{self, LANGS};
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Sel {
    pub anchor: usize,
    pub head: usize,
}

impl Sel {
    pub fn caret(p: usize) -> Self {
        Sel { anchor: p, head: p }
    }
    pub fn lo(&self) -> usize {
        self.anchor.min(self.head)
    }
    pub fn hi(&self) -> usize {
        self.anchor.max(self.head)
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Typing,
    Other,
}

/// One step of an edit. A group's changes are a sequence: redo applies
/// them in order (each `at` is valid at that moment), undo in reverse.
struct Change {
    at: usize,
    old: Vec<u8>,
    new: Vec<u8>,
}

struct Group {
    id: u64,
    kind: EditKind,
    changes: Vec<Change>,
    /// Set once a co-editor's change moved this group: the whole step as an
    /// operation on the text it applies to (the inverse for undo-stack
    /// groups, the forward change for redo-stack ones). Replaces `changes`.
    op: Option<Op>,
    before: Vec<Sel>,
    after: Vec<Sel>,
    time: Instant,
}

impl Group {
    /// Undoing this group, as an operation on a text of length `len`.
    fn inverse(&self, len: usize) -> Option<Op> {
        if let Some(o) = &self.op {
            return Some(o.clone());
        }
        let mut op: Option<Op> = None;
        let mut l = len;
        for c in self.changes.iter().rev() {
            let o = Op::replace(l, c.at, c.at + c.new.len(), &c.old);
            l = l - c.new.len() + c.old.len();
            op = Some(match op {
                Some(p) => p.compose(&o)?,
                None => o,
            });
        }
        Some(op.unwrap_or_else(|| Op::identity(len)))
    }

    /// Redoing this group, as an operation on a text of length `len`.
    fn forward(&self, len: usize) -> Option<Op> {
        if let Some(o) = &self.op {
            return Some(o.clone());
        }
        let mut op: Option<Op> = None;
        let mut l = len;
        for c in &self.changes {
            let o = Op::replace(l, c.at, c.at + c.old.len(), &c.new);
            l = l - c.old.len() + c.new.len();
            op = Some(match op {
                Some(p) => p.compose(&o)?,
                None => o,
            });
        }
        Some(op.unwrap_or_else(|| Op::identity(len)))
    }

    fn is_noop(&self) -> bool {
        match &self.op {
            Some(o) => o.changes().is_empty(),
            None => self.changes.is_empty(),
        }
    }
}

fn map_sels(sels: &mut [Sel], op: &Op) {
    for s in sels.iter_mut() {
        s.anchor = op.map_pos(s.anchor, false);
        s.head = op.map_pos(s.head, false);
    }
}

pub struct Buffer {
    pub id: u32,
    pub doc: Doc,
    pub path: Option<PathBuf>,
    pub untitled: usize,
    pub enc: Enc,
    pub eol: Eol,
    pub mixed_eol: bool,
    pub lang: usize,
    pub sels: Vec<Sel>,
    /// Display column the caret tries to keep on vertical moves.
    pub want_col: Option<usize>,
    undo: Vec<Group>,
    redo: Vec<Group>,
    next_id: u64,
    saved_state: u64,
    /// Break typing coalescing at the next edit (caret moved, file saved...).
    pub seal: bool,
    /// Bumped on every text change; layout caches key on it.
    pub gen: u64,
    pub bookmarks: Vec<usize>,
    /// Folded ranges as (header line, last hidden line), sorted by header.
    pub folds: Vec<(usize, usize)>,
    /// Lexer state at the end of each line, valid for a prefix of the file.
    pub states: Vec<u8>,
    pub mtime: Option<SystemTime>,
    pub backup: Option<String>,
    pub backed_up_gen: u64,
    pub tab_size: usize,
    /// Fixed tab title for special buffers (search results).
    pub label: Option<String>,
    /// Background load in progress: (bytes read so far, total bytes).
    /// The buffer is an empty placeholder and must not be edited or saved.
    pub loading: Option<(std::sync::Arc<std::sync::atomic::AtomicU64>, u64)>,
    /// Background save in progress: (bytes written, total). Read-only meanwhile.
    pub saving: Option<(std::sync::Arc<std::sync::atomic::AtomicU64>, u64)>,
    /// Shown as a table instead of text.
    pub grid: Option<Box<crate::csv::Grid>>,
    /// One side of a revision compare; such a buffer is read-only.
    pub diff: Option<Box<crate::diff::DiffView>>,
    /// This Slate holds the team edit lock on the file, since (unix time).
    pub lock_since: Option<u64>,
    /// Someone else holds the team edit lock: the buffer is read-only.
    pub locked_by: Option<String>,
    /// "Someone else is editing this too" was already shown.
    pub team_warned: bool,
    /// Live co-editing session on this file.
    pub collab: Option<Box<crate::collab::Collab>>,
}

pub fn display_name(path: &Option<PathBuf>, untitled: usize) -> String {
    match path {
        Some(p) => p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        None => format!("new {untitled}"),
    }
}

impl Buffer {
    pub fn new(id: u32, text: Vec<u8>) -> Self {
        Buffer {
            id,
            doc: Doc::load(text),
            path: None,
            untitled: 0,
            enc: Enc::Utf8,
            eol: Eol::Crlf,
            mixed_eol: false,
            lang: 0,
            sels: vec![Sel::default()],
            want_col: None,
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 1,
            saved_state: 0,
            seal: true,
            gen: 1,
            bookmarks: Vec::new(),
            folds: Vec::new(),
            states: Vec::new(),
            mtime: None,
            backup: None,
            backed_up_gen: 0,
            tab_size: 4,
            label: None,
            loading: None,
            saving: None,
            grid: None,
            diff: None,
            lock_since: None,
            locked_by: None,
            team_warned: false,
            collab: None,
        }
    }

    /// Compare sides and files someone else locked cannot be edited.
    pub fn read_only(&self) -> bool {
        self.diff.is_some() || self.locked_by.is_some() || self.loading.is_some() || self.saving.is_some() || self.collab.as_ref().map_or(false, |c| c.joining())
    }

    pub fn name(&self) -> String {
        match &self.label {
            Some(l) => l.clone(),
            None => display_name(&self.path, self.untitled),
        }
    }

    fn state_id(&self) -> u64 {
        self.undo.last().map(|g| g.id).unwrap_or(0)
    }

    pub fn dirty(&self) -> bool {
        self.state_id() != self.saved_state
    }

    pub fn mark_saved(&mut self) {
        self.saved_state = self.state_id();
        self.seal = true;
    }

    /// Forces dirty (restored from a backup whose original differs).
    pub fn mark_unsaved(&mut self) {
        self.saved_state = u64::MAX;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn primary(&self) -> Sel {
        *self.sels.last().unwrap()
    }

    pub fn caret(&self) -> usize {
        self.primary().head
    }

    pub fn set_caret(&mut self, p: usize) {
        self.sels = vec![Sel::caret(p.min(self.doc.len()))];
        self.seal = true;
    }

    /// Sorts selections and merges overlapping ones. The primary (last added)
    /// selection stays last.
    pub fn normalize(&mut self) {
        let n = self.doc.len();
        for s in self.sels.iter_mut() {
            s.anchor = s.anchor.min(n);
            s.head = s.head.min(n);
        }
        if self.sels.len() < 2 {
            return;
        }
        let prim = *self.sels.last().unwrap();
        let mut v = self.sels.clone();
        v.sort_by_key(|s| (s.lo(), s.hi()));
        let mut out: Vec<Sel> = Vec::with_capacity(v.len());
        for s in v {
            if let Some(l) = out.last_mut() {
                if s.lo() < l.hi() || (s.lo() == l.hi() && (s.is_empty() || l.is_empty())) {
                    let lo = l.lo().min(s.lo());
                    let hi = l.hi().max(s.hi());
                    *l = if l.head >= l.anchor { Sel { anchor: lo, head: hi } } else { Sel { anchor: hi, head: lo } };
                    continue;
                }
            }
            out.push(s);
        }
        let pi = out.iter().position(|s| s.lo() <= prim.head && prim.head <= s.hi()).unwrap_or(out.len() - 1);
        let p = out.remove(pi);
        out.push(p);
        self.sels = out;
    }

    /// Selections sorted by position (primary may be anywhere).
    pub fn sorted_sels(&self) -> Vec<Sel> {
        let mut v = self.sels.clone();
        v.sort_by_key(|s| s.lo());
        v
    }

    /// Applies non-overlapping `(start, end, replacement)` changes given in
    /// ascending order, records them as one undo step and maps selections.
    pub fn apply(&mut self, mut changes: Vec<(usize, usize, Vec<u8>)>, kind: EditKind) {
        changes.retain(|(a, b, t)| a != b || !t.is_empty());
        if changes.is_empty() || self.read_only() {
            return;
        }
        changes.sort_by_key(|c| c.0);
        let before = self.sels.clone();
        let mut recorded = Vec::with_capacity(changes.len());
        for (a, b, t) in changes.iter().rev() {
            let old = self.doc.text(*a, *b);
            self.raw_replace(*a, &old, t);
            recorded.push(Change { at: *a, old, new: t.clone() });
        }
        let map = |p: usize| -> usize {
            let mut d: isize = 0;
            for (a, b, t) in &changes {
                if p < *a {
                    break;
                }
                if p <= *b {
                    return (*a as isize + d) as usize + t.len();
                }
                d += t.len() as isize - (*b - *a) as isize;
            }
            (p as isize + d) as usize
        };
        for s in self.sels.iter_mut() {
            s.anchor = map(s.anchor);
            s.head = map(s.head);
        }
        self.normalize();
        self.redo.clear();
        let coalesce = !self.seal
            && kind == EditKind::Typing
            && self.undo.last().map(|g| g.kind == EditKind::Typing && g.op.is_none() && g.time.elapsed().as_millis() < 1500).unwrap_or(false);
        if coalesce {
            let g = self.undo.last_mut().unwrap();
            g.changes.extend(recorded);
            g.time = Instant::now();
        } else {
            let id = self.next_id;
            self.next_id += 1;
            self.undo.push(Group { id, kind, changes: recorded, op: None, before, after: Vec::new(), time: Instant::now() });
            if self.undo.len() > 5000 {
                self.undo.remove(0);
            }
        }
        self.seal = kind != EditKind::Typing;
        self.want_col = None;
    }

    /// Applies a co-editor's change. It is not an undo step of ours, so the
    /// undo and redo histories are transformed over it: undo still takes
    /// back exactly our own edits, wherever the other person typed.
    pub fn apply_remote(&mut self, x: &Op) {
        let changes = x.changes();
        if changes.is_empty() || x.base != self.doc.len() {
            return;
        }
        self.transform_history(x);
        if let Some(c) = self.collab.as_mut() {
            c.muted = true;
        }
        for (a, b, t) in changes.iter().rev() {
            let old = self.doc.text(*a, *b);
            self.raw_replace(*a, &old, t);
        }
        if let Some(c) = self.collab.as_mut() {
            c.muted = false;
        }
        // A caret where the other person typed stays before their text.
        for s in self.sels.iter_mut() {
            s.anchor = x.map_pos(s.anchor, false);
            s.head = x.map_pos(s.head, false);
        }
        self.normalize();
        self.mark_unsaved();
        self.seal = true;
    }

    /// Moves every undo and redo step over a change made to the current
    /// text by someone else. Undo step k applies to the text as it is after
    /// undoing steps above it, so `x` is carried down the stack with each
    /// transform (and likewise for redo).
    fn transform_history(&mut self, x: &Op) {
        for redo in [false, true] {
            let mut cur = x.clone();
            let mut len = self.doc.len();
            let stack = if redo { &mut self.redo } else { &mut self.undo };
            let mut ok = true;
            for g in stack.iter_mut().rev() {
                let step = if redo { g.forward(len) } else { g.inverse(len) };
                let Some(step) = step else {
                    ok = false;
                    break;
                };
                let Some((step2, cur2)) = Op::transform(&step, &cur) else {
                    ok = false;
                    break;
                };
                // Selections: `after` lives on the text before this step is
                // undone, `before` on the text after.
                let (on_now, on_next) = if redo { (&mut g.before, &mut g.after) } else { (&mut g.after, &mut g.before) };
                map_sels(on_now, &cur);
                map_sels(on_next, &cur2);
                len = step.target;
                g.op = Some(step2);
                g.changes.clear();
                cur = cur2;
            }
            if !ok {
                // Cannot happen with consistent history; never risk a wrong undo.
                stack.clear();
            }
        }
    }

    /// Replaces the whole text without an undo step (joining a co-editing
    /// session).
    pub fn replace_all_quiet(&mut self, text: Vec<u8>) {
        self.doc = Doc::load(text);
        self.gen += 1;
        self.states.clear();
        self.folds.clear();
        let lines = self.doc.line_count();
        self.bookmarks.retain(|&l| l < lines);
        self.undo.clear();
        self.redo.clear();
        self.normalize();
        self.seal = true;
        if let Some(mut g) = self.grid.take() {
            g.reindex(&mut self.doc, 0);
            self.grid = Some(g);
        }
    }

    fn raw_replace(&mut self, at: usize, old: &[u8], new: &[u8]) {
        if let Some(c) = self.collab.as_mut() {
            if !c.muted {
                c.outbox.push((self.doc.len(), at, at + old.len(), new.to_vec()));
            }
        }
        let line = self.doc.line_of(at);
        self.doc.remove_at(at, old.len());
        self.doc.insert_at(at, new);
        self.gen += 1;
        let removed = old.iter().filter(|&&c| c == b'\n').count();
        let added = new.iter().filter(|&&c| c == b'\n').count();
        self.states.truncate(line);
        if removed != 0 || added != 0 {
            let shift = |l: usize| (l + added).saturating_sub(removed);
            for bm in self.bookmarks.iter_mut() {
                if *bm > line + removed {
                    *bm = shift(*bm);
                } else if *bm > line {
                    *bm = line;
                }
            }
            self.bookmarks.dedup();
        }
        self.folds.retain(|&(h, e)| e < line || h > line + removed);
        if removed != added {
            for f in self.folds.iter_mut() {
                if f.0 > line {
                    f.0 = (f.0 + added) - removed;
                    f.1 = (f.1 + added) - removed;
                }
            }
        }
    }

    pub fn undo(&mut self) -> bool {
        if self.read_only() {
            return false;
        }
        // Steps a co-editor's change emptied (they deleted our text) are gone.
        while self.undo.last().map_or(false, |g| g.is_noop()) {
            self.undo.pop();
        }
        let Some(mut g) = self.undo.pop() else { return false };
        g.after = self.sels.clone();
        if let Some(inv) = g.op.take() {
            // Redo then re-applies what this removes, lowest change first.
            let ch = inv.changes();
            let mut removed = Vec::with_capacity(ch.len());
            for (a, b, t) in ch.iter().rev() {
                let old = self.doc.text(*a, *b);
                self.raw_replace(*a, &old, t);
                removed.push(old);
            }
            removed.reverse();
            g.changes = ch.into_iter().zip(removed).map(|((a, _, t), r)| Change { at: a, old: t, new: r }).collect();
        } else {
            for c in g.changes.iter().rev() {
                let new = c.new.clone();
                self.raw_replace(c.at, &new, &c.old);
            }
        }
        self.sels = g.before.clone();
        self.normalize();
        self.redo.push(g);
        self.seal = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.read_only() {
            return false;
        }
        while self.redo.last().map_or(false, |g| g.is_noop()) {
            self.redo.pop();
        }
        let Some(mut g) = self.redo.pop() else { return false };
        if let Some(fwd) = g.op.take() {
            // As a sequence for undo: each change at its place once the
            // lower ones are made.
            let ch = fwd.changes();
            let mut removed = Vec::with_capacity(ch.len());
            for (a, b, t) in ch.iter().rev() {
                let old = self.doc.text(*a, *b);
                self.raw_replace(*a, &old, t);
                removed.push(old);
            }
            removed.reverse();
            let mut shift: isize = 0;
            g.changes = ch
                .into_iter()
                .zip(removed)
                .map(|((a, b, t), r)| {
                    let at = (a as isize + shift) as usize;
                    shift += t.len() as isize - (b - a) as isize;
                    Change { at, old: r, new: t }
                })
                .collect();
        } else {
            for c in g.changes.iter() {
                self.raw_replace(c.at, &c.old, &c.new);
            }
        }
        self.sels = g.after.clone();
        self.normalize();
        self.undo.push(g);
        self.seal = true;
        true
    }

    // ---- line geometry ------------------------------------------------------

    pub fn line_col(&self, p: usize) -> (usize, usize) {
        let l = self.doc.line_of(p);
        (l, p - self.doc.line_start(l))
    }

    /// Display column of byte `p` (tabs expanded, one cell per character).
    pub fn display_col(&self, p: usize) -> usize {
        let l = self.doc.line_of(p);
        let mut col = 0;
        let mut q = self.doc.line_start(l);
        while q < p {
            let c = self.doc.byte(q);
            if c == b'\t' {
                col = (col / self.tab_size + 1) * self.tab_size;
            } else if c & 0xC0 != 0x80 {
                col += 1;
            }
            q += 1;
        }
        col
    }

    /// Byte offset on `line` closest to display column `col`.
    pub fn pos_at_col(&self, line: usize, col: usize) -> usize {
        let (s, e) = (self.doc.line_start(line), self.doc.line_end(line));
        let mut c = 0;
        let mut q = s;
        while q < e {
            let b = self.doc.byte(q);
            let next = if b == b'\t' { (c / self.tab_size + 1) * self.tab_size } else { c + 1 };
            if next > col {
                if col - c > (next - c) / 2 {
                    q = self.doc.next_char(q);
                }
                return q;
            }
            c = next;
            q = self.doc.next_char(q);
        }
        e
    }

    pub fn line_indent(&self, line: usize) -> (usize, Option<usize>) {
        let (s, e) = (self.doc.line_start(line), self.doc.line_end(line));
        let mut w = 0;
        let mut q = s;
        while q < e {
            match self.doc.byte(q) {
                b' ' => w += 1,
                b'\t' => w = (w / self.tab_size + 1) * self.tab_size,
                _ => return (q - s, Some(w)),
            }
            q += 1;
        }
        (q - s, None)
    }

    // ---- folding ------------------------------------------------------------

    /// Last line of the block headed by `line`, judged by indentation.
    pub fn fold_end(&self, line: usize) -> Option<usize> {
        // Folding one side of a compare would break the line alignment.
        if self.diff.is_some() {
            return None;
        }
        let n = self.doc.line_count();
        let base = self.line_indent(line).1?;
        let mut k = line + 1;
        let mut first = None;
        while k < n {
            if let Some(w) = self.line_indent(k).1 {
                first = Some((k, w));
                break;
            }
            k += 1;
        }
        let (mut k, w) = first?;
        // Allman style: a lone "{" at the header's indent opens the block.
        let allman = w == base && {
            let s = self.doc.line_start(k) + self.line_indent(k).0;
            self.doc.byte(s) == b'{' && s + 1 == self.doc.line_end(k)
        };
        if w <= base && !allman {
            return None;
        }
        let mut last = k;
        k += 1;
        while k < n {
            match self.line_indent(k).1 {
                Some(w) if w <= base => {
                    if allman {
                        let s = self.doc.line_start(k) + self.line_indent(k).0;
                        if self.doc.byte(s) == b'}' {
                            last = k;
                        }
                    }
                    break;
                }
                Some(_) => last = k,
                None => {}
            }
            k += 1;
        }
        Some(last)
    }

    pub fn is_folded(&self, line: usize) -> bool {
        self.folds.iter().any(|f| f.0 == line)
    }

    pub fn toggle_fold(&mut self, line: usize) {
        if let Some(i) = self.folds.iter().position(|f| f.0 == line) {
            self.folds.remove(i);
        } else if let Some(e) = self.fold_end(line) {
            self.folds.push((line, e));
            self.folds.sort();
            // Carets inside the hidden block move to its header.
            let hs = self.doc.line_end(line);
            for s in self.sels.iter_mut() {
                let l = self.doc.line_of(s.head);
                if l > line && l <= e {
                    *s = Sel::caret(hs);
                }
            }
            self.normalize();
        }
    }

    /// Outermost fold hiding `line`, if any.
    pub fn hidden_by(&self, line: usize) -> Option<usize> {
        self.folds.iter().filter(|f| f.0 < line && line <= f.1).map(|f| f.0).min()
    }

    pub fn reveal(&mut self, line: usize) {
        self.folds.retain(|f| !(f.0 < line && line <= f.1));
    }

    pub fn next_visible(&self, line: usize) -> Option<usize> {
        let mut l = line;
        if let Some(f) = self.folds.iter().filter(|f| f.0 == l).map(|f| f.1).max() {
            l = f;
        }
        let n = l + 1;
        if n >= self.doc.line_count() {
            return None;
        }
        Some(match self.hidden_by(n) {
            Some(h) => self.folds.iter().filter(|f| f.0 == h).map(|f| f.1).max().unwrap_or(n) + 1,
            None => n,
        })
        .filter(|&v| v < self.doc.line_count())
    }

    pub fn prev_visible(&self, line: usize) -> Option<usize> {
        if line == 0 {
            return None;
        }
        let p = line - 1;
        Some(self.hidden_by(p).unwrap_or(p))
    }

    pub fn fold_all(&mut self) {
        self.folds.clear();
        let n = self.doc.line_count();
        let mut l = 0;
        while l < n {
            if let Some(e) = self.fold_end(l) {
                self.folds.push((l, e));
                l = e + 1;
            } else {
                l += 1;
            }
        }
        let caret_line = self.doc.line_of(self.caret());
        if let Some(h) = self.hidden_by(caret_line) {
            self.set_caret(self.doc.line_start(h));
        }
    }

    // ---- syntax state -------------------------------------------------------

    /// Lexer state entering `line`.
    pub fn state_before(&mut self, line: usize) -> u8 {
        if line == 0 {
            return 0;
        }
        let lang = &LANGS[self.lang];
        if lang.kind == syntax::Kind::Plain {
            return 0;
        }
        let mut buf = Vec::new();
        while self.states.len() < line {
            let i = self.states.len();
            let st = if i == 0 { 0 } else { self.states[i - 1] };
            self.doc.line_bytes(i, &mut buf);
            self.states.push(syntax::lex(lang, &buf, st, None));
        }
        self.states[line - 1]
    }

    pub fn set_lang(&mut self, lang: usize) {
        self.lang = lang;
        self.states.clear();
        self.gen += 1;
    }

    // ---- bookmarks ----------------------------------------------------------

    pub fn toggle_bookmark(&mut self, line: usize) {
        match self.bookmarks.binary_search(&line) {
            Ok(i) => {
                self.bookmarks.remove(i);
            }
            Err(i) => self.bookmarks.insert(i, line),
        }
    }

    pub fn text_all(&self) -> Vec<u8> {
        self.doc.text(0, self.doc.len())
    }

    pub fn word_at(&self, p: usize) -> (usize, usize) {
        let is_w = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80;
        let n = self.doc.len();
        let mut a = p;
        while a > 0 && is_w(self.doc.byte(a - 1)) {
            a -= 1;
        }
        let mut b = p;
        while b < n && is_w(self.doc.byte(b)) {
            b += 1;
        }
        (a, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(b: &Buffer) -> String {
        String::from_utf8(b.text_all()).unwrap()
    }

    #[test]
    fn multi_caret_insert_and_undo() {
        let mut b = Buffer::new(1, b"ab\ncd\nef".to_vec());
        b.sels = vec![Sel::caret(1), Sel::caret(4), Sel::caret(7)];
        let ch: Vec<_> = b.sorted_sels().iter().map(|s| (s.lo(), s.hi(), b"X".to_vec())).collect();
        b.apply(ch, EditKind::Typing);
        assert_eq!(text(&b), "aXb\ncXd\neXf");
        assert_eq!(b.sels.iter().map(|s| s.head).collect::<Vec<_>>(), vec![2, 6, 10]);
        assert!(b.dirty());
        b.undo();
        assert_eq!(text(&b), "ab\ncd\nef");
        assert!(!b.dirty());
        b.redo();
        assert_eq!(text(&b), "aXb\ncXd\neXf");
    }

    #[test]
    fn typing_coalesces_and_save_breaks_it() {
        let mut b = Buffer::new(1, Vec::new());
        for c in b"abc" {
            let p = b.caret();
            b.apply(vec![(p, p, vec![*c])], EditKind::Typing);
        }
        b.mark_saved();
        let p = b.caret();
        b.apply(vec![(p, p, b"d".to_vec())], EditKind::Typing);
        assert!(b.dirty());
        b.undo();
        assert_eq!(text(&b), "abc");
        assert!(!b.dirty());
        b.undo();
        assert_eq!(text(&b), "");
    }

    #[test]
    fn bookmarks_and_folds_follow_edits() {
        let mut b = Buffer::new(1, b"a\nb\nc\nd\n".to_vec());
        b.toggle_bookmark(3);
        b.apply(vec![(0, 0, b"x\ny\n".to_vec())], EditKind::Other);
        assert_eq!(b.bookmarks, vec![5]);
        b.apply(vec![(0, 4, Vec::new())], EditKind::Other);
        assert_eq!(b.bookmarks, vec![3]);
    }

    #[test]
    fn fold_by_indent_and_allman() {
        let mut b = Buffer::new(1, b"fn a() {\n    x;\n    y;\n}\nvoid f()\n{\n  z;\n}\n".to_vec());
        assert_eq!(b.fold_end(0), Some(2));
        assert_eq!(b.fold_end(4), Some(7));
        b.toggle_fold(0);
        assert_eq!(b.next_visible(0), Some(3));
        assert_eq!(b.prev_visible(3), Some(0));
        assert!(b.fold_end(1).is_none());
    }

    #[test]
    fn display_columns_with_tabs() {
        let b = Buffer::new(1, b"\tab\n".to_vec());
        assert_eq!(b.display_col(1), 4);
        assert_eq!(b.pos_at_col(0, 5), 2);
        assert_eq!(b.pos_at_col(0, 1), 0);
    }

    fn rnd(seed: &mut u64) -> usize {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 11) as usize
    }

    fn random_edit(b: &mut Buffer, seed: &mut u64, tag: u8) -> Op {
        let len = b.doc.len();
        let a = rnd(seed) % (len + 1);
        let z = (a + rnd(seed) % 4).min(len);
        let t = vec![tag; rnd(seed) % 3];
        let op = Op::replace(len, a, z, &t);
        b.seal = true;
        b.apply(vec![(a, z, t)], EditKind::Other);
        op
    }

    fn random_remote(text: &[u8], seed: &mut u64) -> Op {
        let len = text.len();
        let a = rnd(seed) % (len + 1);
        let z = (a + rnd(seed) % 4).min(len);
        Op::replace(len, a, z, b"#")
    }

    #[test]
    fn undo_takes_back_only_own_edits_after_remote_changes() {
        let mut seed = 0x1234_5678u64;
        for _ in 0..400 {
            let mut b = Buffer::new(1, b"PIPE
ELBOW 90
FLANGE
".to_vec());
            let t0 = b.text_all();
            let n = 1 + rnd(&mut seed) % 5;
            let mut lc = Op::identity(t0.len());
            for k in 0..n {
                let o = random_edit(&mut b, &mut seed, b'a' + k as u8);
                lc = lc.compose(&o).unwrap();
            }
            let t1 = b.text_all();
            let r = random_remote(&t1, &mut seed);
            b.apply_remote(&r);
            let after_remote = b.text_all();
            for _ in 0..n {
                b.undo();
            }
            let inv = lc.invert(&t0);
            let (inv2, _) = Op::transform(&inv, &r).unwrap();
            let expected = inv2.apply(&r.apply(&t1).unwrap()).unwrap();
            // Where their text lands against text an undo puts back at the
            // very same spot may differ (both are right); everything else
            // must match exactly, and none of their text may be lost.
            let strip = |t: &[u8]| t.iter().copied().filter(|&c| c != b'#').collect::<Vec<u8>>();
            let got = b.text_all();
            assert_eq!(String::from_utf8_lossy(&strip(&got)), String::from_utf8_lossy(&strip(&expected)));
            assert_eq!(got.iter().filter(|&&c| c == b'#').count(), expected.iter().filter(|&&c| c == b'#').count());
            for _ in 0..n {
                b.redo();
            }
            assert_eq!(b.text_all(), after_remote);
        }
    }

    #[test]
    fn interleaved_remote_changes_keep_their_text() {
        let mut seed = 0xBEEFu64;
        for _ in 0..300 {
            let mut b = Buffer::new(1, b"0123456789abcdef".to_vec());
            let mut mine = 0;
            for k in 0..12 {
                if rnd(&mut seed) % 2 == 0 {
                    random_edit(&mut b, &mut seed, b'A' + (k % 26) as u8);
                    mine += 1;
                } else {
                    let t = b.text_all();
                    let r = random_remote(&t, &mut seed);
                    b.apply_remote(&r);
                }
                if rnd(&mut seed) % 4 == 0 && b.undo() {
                    mine -= 1;
                }
            }
            let before = b.text_all();
            let mut undone = 0;
            while b.undo() {
                undone += 1;
            }
            assert!(undone <= mine + 12);
            // None of our capital letters survive a full undo.
            assert!(!b.text_all().iter().any(|c| c.is_ascii_uppercase()), "{}", String::from_utf8_lossy(&b.text_all()));
            for _ in 0..undone {
                b.redo();
            }
            assert_eq!(b.text_all(), before);
        }
    }
}
