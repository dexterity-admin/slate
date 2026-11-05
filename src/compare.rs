//! Revision compare UI: two read-only aligned sides in the split views,
//! scrolled together, with change navigation and refresh.

use crate::app::{App, PalAct, PalItem, PalMode, Tab};
use crate::buffer::{Buffer, Sel};
use crate::diff::{self, DiffView, Opts, Src};
use crate::settings;
use crate::sys;
use std::path::PathBuf;

/// Row colours: removed (left) and added (right).
pub const RED: u32 = 0xF85149;
pub const GREEN: u32 = 0x3FB950;
const GREY: u32 = 0x8B949E;
/// Largest file compared side by side.
const CMP_MAX: usize = 256 << 20;

fn opts() -> Opts {
    let s = settings::get();
    Opts { ws: s.cmp_ws, case: s.cmp_case }
}

impl App {
    /// Text, title and language of a compare source.
    fn cmp_source(&self, src: &Src) -> Result<(Vec<u8>, String, usize), String> {
        match src {
            Src::Buf(id) => {
                let b = self.bufs.iter().find(|b| b.id == *id).ok_or("A compared file was closed")?;
                if b.loading.is_some() {
                    return Err("Wait until the file has loaded".into());
                }
                Ok((b.text_all(), b.name(), b.lang))
            }
            Src::File(p) => {
                let raw = crate::doc::read_for_doc(p).map_err(|e| format!("Cannot open {}: {e}", p.display()))?;
                let mut name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                // The file is also open in a tab: this side is its disk copy.
                if self.find_open(p).is_some() {
                    name += " (saved)";
                }
                Ok((crate::enc::decode(raw, None).text, name, crate::syntax::lang_for_path(&p.to_string_lossy())))
            }
        }
    }

    /// Buffer indices (left, right) of the compare the user is looking at.
    fn cmp_pair(&self) -> Option<(usize, usize)> {
        let pick = |i: usize| {
            let d = self.bufs[i].diff.as_ref()?;
            let j = self.bufs.iter().position(|b| b.id == d.partner)?;
            Some(if d.left { (i, j) } else { (j, i) })
        };
        let cur = self.bi(self.tab(self.av).buf);
        pick(cur).or_else(|| (0..self.bufs.len()).find_map(pick))
    }

    pub fn in_compare(&self) -> bool {
        self.cur_ref().diff.is_some()
    }

    pub fn has_compare(&self) -> bool {
        self.cmp_pair().is_some()
    }

    /// Compares two sources and shows them side by side, replacing any
    /// compare already open.
    pub fn compare(&mut self, l: Src, r: Src) {
        let (ta, na, la) = match self.cmp_source(&l) {
            Ok(x) => x,
            Err(e) => return self.flash(&e),
        };
        let (tb, nb, lb) = match self.cmp_source(&r) {
            Ok(x) => x,
            Err(e) => return self.flash(&e),
        };
        // Both aligned copies live in memory next to the originals.
        if ta.len().max(tb.len()) > CMP_MAX {
            return self.flash("These files are too large to compare side by side (256 MB each at most)");
        }
        self.close_compare();
        let o = opts();
        self.cmp_busy(ta.len() + tb.len());
        let al = diff::align(&ta, &tb, o);
        let lid = self.new_buffer(Vec::new());
        let rid = self.new_buffer(Vec::new());
        let (li, ri) = (self.bi(lid), self.bi(rid));
        for (i, name, lang) in [(li, na, la), (ri, nb, lb)] {
            let b = &mut self.bufs[i];
            b.label = Some(format!("\u{21C4} {name}"));
            b.lang = lang;
        }
        self.cmp_install(li, ri, al, l, r, o);
        // The compare takes both views: left side in view 0, right in view 1.
        self.split = true;
        self.add_tab(0, lid);
        self.add_tab(1, rid);
        self.av = 0;
        self.cmp_sync = None;
        self.cmp_after();
    }

    /// Puts an alignment into the two compare buffers.
    fn cmp_install(&mut self, li: usize, ri: usize, al: diff::Aligned, l: Src, r: Src, o: Opts) {
        let (lid, rid) = (self.bufs[li].id, self.bufs[ri].id);
        let diff::Aligned { left, right, kinds, blocks, stats } = al;
        for (i, side, partner, src, is_left) in [(li, left, rid, l, true), (ri, right, lid, r, false)] {
            let mut nb = Buffer::new(self.bufs[i].id, side.text);
            let old = &self.bufs[i];
            nb.label = old.label.clone();
            nb.lang = old.lang;
            nb.tab_size = old.tab_size;
            nb.eol = crate::enc::Eol::Lf;
            nb.diff = Some(Box::new(DiffView {
                partner,
                left: is_left,
                src,
                kinds: kinds.clone(),
                nums: side.nums,
                marks: side.marks,
                blocks: blocks.clone(),
                stats,
                opts: o,
            }));
            self.bufs[i] = nb;
        }
    }

    /// Shows "Comparing…" before a compare that takes a noticeable time.
    fn cmp_busy(&mut self, bytes: usize) {
        if bytes > 8 << 20 {
            self.flash("Comparing\u{2026}");
            self.paint();
        }
    }

    /// Jumps to the first change and reports the result.
    fn cmp_after(&mut self) {
        let Some((li, _)) = self.cmp_pair() else { return };
        let d = self.bufs[li].diff.as_ref().unwrap();
        let mut ignoring = Vec::new();
        if d.opts.ws {
            ignoring.push("whitespace");
        }
        if d.opts.case {
            ignoring.push("case");
        }
        let ign = if ignoring.is_empty() { String::new() } else { format!(" (ignoring {})", ignoring.join(" and ")) };
        if d.blocks.is_empty() {
            self.flash(&format!("The files are identical{ign}"));
        } else {
            let n = d.blocks.len();
            self.flash(&format!("{n} difference{}{ign} \u{2013} F7 / Shift+F7 move between them", if n == 1 { "" } else { "s" }));
            self.cmp_goto_block(0);
        }
        self.after_switch();
    }

    /// Recompares the open compare from its sources (after editing them).
    pub fn cmp_refresh(&mut self) {
        let Some((li, ri)) = self.cmp_pair() else { return };
        let (l, r) = {
            let (dl, dr) = (self.bufs[li].diff.as_ref().unwrap(), self.bufs[ri].diff.as_ref().unwrap());
            (dl.src.clone(), dr.src.clone())
        };
        self.cmp_recompute(li, ri, l, r);
    }

    fn cmp_recompute(&mut self, li: usize, ri: usize, l: Src, r: Src) {
        let (ta, na, _) = match self.cmp_source(&l) {
            Ok(x) => x,
            Err(e) => return self.flash(&e),
        };
        let (tb, nb, _) = match self.cmp_source(&r) {
            Ok(x) => x,
            Err(e) => return self.flash(&e),
        };
        let o = opts();
        self.cmp_busy(ta.len() + tb.len());
        let al = diff::align(&ta, &tb, o);
        self.bufs[li].label = Some(format!("\u{21C4} {na}"));
        self.bufs[ri].label = Some(format!("\u{21C4} {nb}"));
        let line = {
            let b = self.cur_ref();
            b.doc.line_of(b.caret())
        };
        self.cmp_install(li, ri, al, l, r, o);
        for i in [li, ri] {
            let b = &mut self.bufs[i];
            let l = line.min(b.doc.line_count() - 1);
            let p = b.doc.line_start(l);
            b.set_caret(p);
        }
        self.cmp_sync = None;
        self.cmp_after_refresh();
    }

    fn cmp_after_refresh(&mut self) {
        let Some((li, _)) = self.cmp_pair() else { return };
        let n = self.bufs[li].diff.as_ref().unwrap().blocks.len();
        self.flash(&if n == 0 { "Compare refreshed: the files are identical".to_string() } else { format!("Compare refreshed: {n} difference{}", if n == 1 { "" } else { "s" }) });
        self.after_switch();
    }

    pub fn cmp_swap(&mut self) {
        let Some((li, ri)) = self.cmp_pair() else { return };
        let (l, r) = (self.bufs[li].diff.as_ref().unwrap().src.clone(), self.bufs[ri].diff.as_ref().unwrap().src.clone());
        self.cmp_recompute(li, ri, r, l);
    }

    /// Closes both compare tabs.
    pub fn close_compare(&mut self) {
        while let Some(id) = self.bufs.iter().find(|b| b.diff.is_some()).map(|b| b.id) {
            match self.find_open_id(id) {
                Some((v, i)) => {
                    self.close_tab(v, i);
                }
                None => self.bufs.retain(|b| b.id != id),
            }
        }
    }

    /// Ctrl+Alt+C: closes an open compare; compares the two views when both
    /// show a file; otherwise asks what to compare the current file with.
    pub fn compare_with(&mut self) {
        if self.in_compare() {
            self.close_compare();
            return;
        }
        let cur = self.cur_ref().id;
        if self.split && !self.views[1].tabs.is_empty() {
            let (a, b) = (self.tab(0).buf, self.tab(1).buf);
            let plain = |id: u32| self.bufs[self.bi(id)].diff.is_none();
            if a != b && plain(a) && plain(b) {
                return self.compare(Src::Buf(a), Src::Buf(b));
            }
        }
        let mut items = Vec::new();
        let b = self.cur_ref();
        if b.path.is_some() && b.dirty() {
            items.push(PalItem { label: "Saved version on disk".into(), hint: "unsaved changes".into(), act: PalAct::Cmd(crate::app::Cmd::CompareSaved) });
        }
        for v in 0..2 {
            for t in &self.views[v].tabs {
                let o = &self.bufs[self.bi(t.buf)];
                if o.id == cur || o.diff.is_some() || o.loading.is_some() || items.iter().any(|it: &PalItem| matches!(it.act, PalAct::Compare(x) if x == o.id)) {
                    continue;
                }
                let hint = o.path.as_ref().and_then(|p| p.parent()).map(|p| p.display().to_string()).unwrap_or_default();
                items.push(PalItem { label: o.name(), hint, act: PalAct::Compare(o.id) });
            }
        }
        items.push(PalItem { label: "Browse for a file\u{2026}".into(), hint: String::new(), act: PalAct::Cmd(crate::app::Cmd::CompareFile) });
        let title = format!("Compare {} with\u{2026}", self.cur_ref().name());
        self.open_palette(PalMode::List, &title, items, "");
    }

    /// The other file goes on the left (older), the current one on the right.
    pub fn compare_buf(&mut self, other: u32) {
        let cur = self.cur_ref().id;
        self.compare(Src::Buf(other), Src::Buf(cur));
    }

    pub fn compare_file(&mut self) {
        let cur = self.cur_ref();
        if cur.diff.is_some() {
            return;
        }
        let id = cur.id;
        let dir = cur.path.as_ref().and_then(|p| p.parent()).map(|p| p.to_string_lossy().into_owned());
        if let Some(f) = sys::open_dialog(self.hwnd, dir.as_deref()).into_iter().next() {
            self.compare(Src::File(PathBuf::from(f)), Src::Buf(id));
        }
    }

    pub fn compare_saved(&mut self) {
        let b = self.cur_ref();
        let Some(p) = b.path.clone().filter(|_| b.diff.is_none()) else {
            return self.flash("This file has not been saved yet");
        };
        let id = b.id;
        self.compare(Src::File(p), Src::Buf(id));
    }

    pub fn cmp_toggle_opt(&mut self, case: bool) {
        let on = {
            let mut s = settings::get_mut();
            let f = if case { &mut s.cmp_case } else { &mut s.cmp_ws };
            *f = !*f;
            *f
        };
        let what = if case { "case" } else { "whitespace" };
        if self.cmp_pair().is_some() {
            self.cmp_refresh();
        }
        self.flash(&format!("Compare {} {what}", if on { "ignores" } else { "shows" }));
    }

    /// Moves both sides to change block `k`.
    fn cmp_goto_block(&mut self, k: usize) {
        let Some((li, ri)) = self.cmp_pair() else { return };
        let Some(&(s, _)) = self.bufs[li].diff.as_ref().unwrap().blocks.get(k) else { return };
        for i in [li, ri] {
            let b = &mut self.bufs[i];
            let p = b.doc.line_start(s as usize);
            b.set_caret(p);
        }
        self.cmp_sync = None;
        self.center_caret();
        // Keep a little context above the change rather than centring.
        let v = self.av;
        let vis = self.vis_rows(v);
        let t = self.tab_mut(v);
        t.top = (s as usize).saturating_sub(vis / 4);
        t.sub = 0;
        self.redraw();
    }

    pub fn cmp_step(&mut self, forward: bool) {
        if !self.in_compare() {
            return self.flash("Open a compare first \u{2013} Ctrl+Alt+C");
        }
        let b = self.cur_ref();
        let d = b.diff.as_ref().unwrap();
        let line = b.doc.line_of(b.caret()) as u32;
        let k = if forward {
            d.blocks.iter().position(|&(s, _)| s > line)
        } else {
            d.blocks.iter().rposition(|&(_, e)| e <= line)
        };
        match k {
            Some(k) => self.cmp_goto_block(k),
            None => self.flash(if d.blocks.is_empty() { "No differences" } else if forward { "No more differences below" } else { "No more differences above" }),
        }
    }

    /// Keeps the two compare sides scrolled together: whichever moved last
    /// leads.
    pub fn cmp_sync_views(&mut self) {
        if !self.split || self.views[1].tabs.is_empty() {
            return;
        }
        let paired = match (&self.buf_of(0).diff, &self.buf_of(1).diff) {
            (Some(d), Some(_)) => d.partner == self.buf_of(1).id,
            _ => false,
        };
        if !paired {
            self.cmp_sync = None;
            return;
        }
        let st = |t: &Tab| (t.top, t.sub, t.x);
        let (s0, s1) = (st(self.tab(0)), st(self.tab(1)));
        if s0 == s1 {
            self.cmp_sync = Some(s0);
            return;
        }
        let lead = if self.cmp_sync == Some(s0) {
            1
        } else if self.cmp_sync == Some(s1) {
            0
        } else {
            self.av
        };
        let s = if lead == 0 { s0 } else { s1 };
        let t = self.tab_mut(1 - lead);
        (t.top, t.sub, t.x) = s;
        self.scroll_anim[1 - lead] = self.scroll_anim[lead];
        self.cmp_sync = Some(s);
        // The caret line follows too, so both sides highlight the same row.
        let line = {
            let b = self.buf_of(lead);
            b.doc.line_of(b.caret())
        };
        let ob = self.buf_of_mut(1 - lead);
        if ob.doc.line_of(ob.caret()) != line && line < ob.doc.line_count() {
            let p = ob.doc.line_start(line);
            ob.sels = vec![Sel::caret(p)];
        }
    }

    /// Status text for a compare tab: (summary, position).
    pub fn cmp_status(&self) -> Option<(String, String)> {
        let b = self.cur_ref();
        let d = b.diff.as_ref()?;
        let line = b.doc.line_of(b.caret());
        let st = d.stats;
        let mut parts = Vec::new();
        if d.blocks.is_empty() {
            parts.push("Identical".to_string());
        } else {
            let at = d.blocks.iter().position(|&(s, e)| (s as usize) <= line && line < e as usize);
            parts.push(match at {
                Some(k) => format!("Difference {} of {}", k + 1, d.blocks.len()),
                None => format!("{} differences", d.blocks.len()),
            });
            if st.changed > 0 {
                parts.push(format!("{} changed", st.changed));
            }
            if st.added > 0 {
                parts.push(format!("{} added", st.added));
            }
            if st.removed > 0 {
                parts.push(format!("{} removed", st.removed));
            }
        }
        if d.opts.ws {
            parts.push("ignoring whitespace".into());
        }
        if d.opts.case {
            parts.push("ignoring case".into());
        }
        let pos = match d.nums.get(line).copied().unwrap_or(0) {
            0 => "Ln \u{2013}".to_string(),
            n => format!("Ln {n}, Col {}", b.display_col(b.caret()) + 1),
        };
        Some((parts.join("  \u{B7}  "), pos))
    }

    /// Background of one compare row, drawn under the text.
    pub fn paint_diff_row(&self, d: &DiffView, line: usize, x: f32, y: f32, w: f32, h: f32) {
        let g = &self.gfx;
        let k = d.kinds.get(line).copied().unwrap_or(diff::SAME);
        if k == diff::SAME {
            return;
        }
        if d.is_filler(line) {
            // Hatched: nothing on this side.
            g.fill_tint(x, y, w, h, GREY, 0.05);
            g.clip(x, y, w, h);
            let step = 9.0;
            let mut sx = x - h + ((y / step).fract() * step);
            while sx < x + w {
                g.line_tint(sx, y + h, sx + h, y, GREY, 0.22, 1.0);
                sx += step;
            }
            g.unclip();
            return;
        }
        let rgb = if d.left { RED } else { GREEN };
        g.fill_tint(x, y, w, h, rgb, if g.dark { 0.13 } else { 0.11 });
    }

    /// Marker bar and colour for the gutter of a compare row.
    pub fn paint_diff_gutter(&self, d: &DiffView, line: usize, x: f32, y: f32, h: f32) {
        let k = d.kinds.get(line).copied().unwrap_or(diff::SAME);
        if k == diff::SAME || d.is_filler(line) {
            return;
        }
        let rgb = if d.left { RED } else { GREEN };
        self.gfx.fill_tint(x, y, 3.0, h, rgb, 0.9);
    }

    /// Change ticks along the scrollbar track.
    pub fn paint_diff_ticks(&self, d: &DiffView, x: f32, ty: f32, th: f32) {
        let n = d.kinds.len().max(1) as f32;
        let mut last = -10.0f32;
        for &(s, e) in &d.blocks {
            let y = ty + th * s as f32 / n;
            let hgt = (th * (e - s) as f32 / n).max(2.0);
            if y + hgt < last + 1.0 {
                continue;
            }
            last = y + hgt;
            let k = d.kinds[s as usize];
            let rgb = match k {
                diff::LEFT => RED,
                diff::RIGHT => GREEN,
                _ => 0xD29922,
            };
            self.gfx.fill_tint(x + 2.0, y, SCROLL_TICK_W, hgt, rgb, 0.85);
        }
    }
}

const SCROLL_TICK_W: f32 = crate::app::SCROLL_W - 4.0;
