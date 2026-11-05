//! View geometry, the per-line DirectWrite layout cache, scrolling and
//! point <-> text position mapping.

use crate::app::{App, LineLayout, SCROLL_W, STATUS_H, TAB_H};
use crate::gfx::C;
use crate::settings;
use crate::syntax::{self, Tok, LANGS};
use std::hash::{Hash, Hasher};
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_HIT_TEST_METRICS, DWRITE_TEXT_METRICS,
    DWRITE_TEXT_RANGE,
};

/// Longest line prefix laid out; the rest of a giant line is not drawn.
pub const MAX_LAYOUT_BYTES: usize = 32 * 1024;

pub struct Row {
    pub line: usize,
    pub key: u64,
    pub y: f32,
    pub rows: usize,
}

impl App {
    pub fn view_rect(&self, v: usize) -> (f32, f32, f32, f32) {
        let y = self.top();
        let h = (self.h - y - STATUS_H).max(0.0);
        // The AI panel, when open, takes the right side.
        let w = self.ai_rect().map_or(self.w, |p| p.x);
        if !self.split {
            return (0.0, y, w, h);
        }
        let sx = (w * self.split_frac).round();
        if v == 0 {
            (0.0, y, sx, h)
        } else {
            (sx + 1.0, y, w - sx - 1.0, h)
        }
    }

    pub fn gutter_w(&self, v: usize) -> f32 {
        let n = self.buf_of(v).doc.line_count();
        let digits = (n.max(1) as f32).log10().floor() as usize + 1;
        let cw = self.gfx.char_w * 0.9;
        16.0 + digits.max(3) as f32 * cw + 20.0
    }

    /// Text area of view `v`: (x, y, w, h).
    pub fn text_rect(&self, v: usize) -> (f32, f32, f32, f32) {
        let (x, y, w, h) = self.view_rect(v);
        let g = self.gutter_w(v);
        (x + g, y + TAB_H, (w - g - SCROLL_W).max(10.0), (h - TAB_H).max(0.0))
    }

    pub fn wrap_width(&self, v: usize) -> Option<f32> {
        // Compare sides never wrap, so their lines stay level.
        (settings::get().wrap && self.buf_of(v).diff.is_none()).then(|| (self.text_rect(v).2 - 6.0).max(40.0))
    }

    pub fn vis_rows(&self, v: usize) -> usize {
        ((self.text_rect(v).3 / self.gfx.line_h).floor() as usize).max(1)
    }

    /// Lays out line `line` of buffer index `bi` and returns its cache key.
    pub fn layout(&mut self, bi: usize, line: usize, wrap: Option<f32>) -> u64 {
        let show_ws = settings::get().show_ws;
        let state = self.bufs[bi].state_before(line);
        let b = &self.bufs[bi];
        let (s, e) = (b.doc.line_start(line), b.doc.line_end(line));
        let mut bytes = Vec::new();
        b.doc.gap.copy_range(s, e.min(s + MAX_LAYOUT_BYTES), &mut bytes);
        while e > s + MAX_LAYOUT_BYTES && !bytes.is_empty() && std::str::from_utf8(&bytes).is_err() {
            bytes.pop();
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        let colored = !self.gfx.brushes.is_empty();
        (state, b.lang, show_ws, self.font_gen, self.gfx.dark, wrap.map(|w| w as u32), colored).hash(&mut h);
        let key = h.finish();
        let tick = self.cache_tick;
        if let Some(l) = self.cache.get_mut(&key) {
            l.used = tick;
            return key;
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut u16s: Vec<u16> = Vec::with_capacity(bytes.len());
        let mut map: Vec<u32> = Vec::with_capacity(bytes.len() + 1);
        let mut off = 0u32;
        let mut buf2 = [0u16; 2];
        for ch in text.chars() {
            let shown = if show_ws && ch == ' ' { '·' } else { ch };
            for u in shown.encode_utf16(&mut buf2).iter() {
                u16s.push(*u);
                map.push(off);
            }
            off += ch.len_utf8() as u32;
        }
        map.push(off);
        let fmt = if wrap.is_some() { self.gfx.mono_wrap.clone() } else { self.gfx.mono.clone() };
        let fmt = fmt.unwrap();
        let lay = unsafe { self.gfx.dw.CreateTextLayout(&u16s, &fmt, wrap.unwrap_or(1.0e6), self.gfx.line_h) };
        let Ok(lay) = lay else { return key };
        let lang = &LANGS[b.lang];
        if lang.kind != syntax::Kind::Plain && !self.gfx.brushes.is_empty() {
            let mut toks = Vec::new();
            syntax::lex(lang, &bytes, state, Some(&mut toks));
            let ll = LineLayout { layout: lay.clone(), map: map.clone(), rows: 1, width: 0.0, used: tick };
            for (a, z, t) in toks {
                let c = match t {
                    Tok::Keyword => C::Kw,
                    Tok::Type => C::Ty,
                    Tok::Str => C::Str,
                    Tok::Num => C::Num,
                    Tok::Comment => C::Com,
                    Tok::Pre => C::Pre,
                    Tok::Tag => C::Tag,
                    Tok::Attr => C::Attr,
                    Tok::Op => C::Op,
                    Tok::Heading => C::Head,
                    Tok::Text => continue,
                };
                let (a16, z16) = (ll.to16(a as usize), ll.to16(z as usize));
                let r = DWRITE_TEXT_RANGE { startPosition: a16, length: z16 - a16 };
                unsafe {
                    let _ = lay.SetDrawingEffect(self.gfx.b(c), r);
                    if t == Tok::Comment {
                        let _ = lay.SetFontStyle(DWRITE_FONT_STYLE_ITALIC, r);
                    } else if t == Tok::Heading {
                        let _ = lay.SetFontWeight(DWRITE_FONT_WEIGHT_SEMI_BOLD, r);
                    }
                }
            }
        }
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            let _ = lay.GetMetrics(&mut m);
        }
        let rows = if wrap.is_some() { m.lineCount.max(1) as usize } else { 1 };
        if self.cache.len() > 400 {
            self.cache.retain(|_, l| l.used + 1 >= tick);
        }
        self.cache.insert(key, LineLayout { layout: lay, map, rows, width: m.widthIncludingTrailingWhitespace, used: tick });
        key
    }

    /// Visible rows of view `v` from its scroll position down.
    pub fn rows(&mut self, v: usize) -> Vec<Row> {
        let bi = self.bi(self.tab(v).buf);
        let (_, ty, _, th) = self.text_rect(v);
        let wrap = self.wrap_width(v);
        let lh = self.gfx.line_h;
        {
            let n = self.bufs[bi].doc.line_count();
            let b = &self.bufs[bi];
            let mut top = self.tab(v).top.min(n - 1);
            if let Some(h) = b.hidden_by(top) {
                top = h;
            }
            let t = self.tab_mut(v);
            if t.top != top {
                t.top = top;
                t.sub = 0;
            }
        }
        let mut line = self.tab(v).top;
        let mut y = ty - self.tab(v).sub as f32 * lh + self.scroll_offset(v);
        // While a smooth scroll settles, lines above the top come into view.
        while y > ty {
            match self.bufs[bi].prev_visible(line) {
                Some(p) => {
                    line = p;
                    y -= self.line_rows(bi, p, wrap) as f32 * lh;
                }
                None => break,
            }
        }
        let mut out = Vec::new();
        while y < ty + th {
            let key = self.layout(bi, line, wrap);
            let rows = self.cache.get(&key).map(|l| l.rows).unwrap_or(1);
            out.push(Row { line, key, y, rows });
            y += rows as f32 * lh;
            match self.bufs[bi].next_visible(line) {
                Some(n) => line = n,
                None => break,
            }
        }
        out
    }

    fn line_rows(&mut self, bi: usize, line: usize, wrap: Option<f32>) -> usize {
        if wrap.is_none() {
            return 1;
        }
        let k = self.layout(bi, line, wrap);
        self.cache.get(&k).map(|l| l.rows).unwrap_or(1)
    }

    /// Layout-relative (x, y) of byte `pos` on `line`.
    pub fn pos_xy(&mut self, bi: usize, line: usize, pos: usize, wrap: Option<f32>) -> (f32, f32) {
        let k = self.layout(bi, line, wrap);
        let Some(l) = self.cache.get(&k) else { return (0.0, 0.0) };
        let rel = pos - self.bufs[bi].doc.line_start(line);
        let i16 = l.to16(rel);
        let (mut x, mut y) = (0.0f32, 0.0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = l.layout.HitTestTextPosition(i16, BOOL(0), &mut x, &mut y, &mut m);
        }
        if rel > *l.map.last().unwrap() as usize {
            x = l.width;
        }
        (x, y)
    }

    /// Current smooth-scroll pixel offset of view `v`.
    pub fn scroll_offset(&self, v: usize) -> f32 {
        self.scroll_anim[v].as_ref().map(|t| t.value()).unwrap_or(0.0)
    }

    /// Scrolls by `delta` rows and glides there instead of jumping.
    pub fn scroll_smooth(&mut self, v: usize, delta: isize) {
        let moved = self.scroll_by(v, delta);
        if moved != 0 {
            let start = self.scroll_offset(v) + moved as f32 * self.gfx.line_h;
            self.scroll_anim[v] = Some(crate::anim::Tween::new(start, 0.0, 160.0));
        }
    }

    /// Scrolls by `delta` rows; returns the rows actually moved.
    pub fn scroll_by(&mut self, v: usize, delta: isize) -> isize {
        let (t0, s0) = (self.tab(v).top, self.tab(v).sub);
        self.scroll_by_inner(v, delta);
        let (t1, s1) = (self.tab(v).top, self.tab(v).sub);
        if (t0, s0) == (t1, s1) {
            return 0;
        }
        // Count rows between the two positions (lines without wrap).
        if self.wrap_width(v).is_none() {
            let bi = self.bi(self.tab(v).buf);
            let (a, b) = (t0.min(t1), t0.max(t1));
            let mut n = 0isize;
            let mut l = a;
            while l < b {
                match self.bufs[bi].next_visible(l) {
                    Some(nx) => l = nx,
                    None => break,
                }
                n += 1;
            }
            if t1 > t0 {
                n
            } else {
                -n
            }
        } else {
            // Wrapped rows vary per line; jump without gliding.
            0
        }
    }

    fn scroll_by_inner(&mut self, v: usize, delta: isize) {
        let bi = self.bi(self.tab(v).buf);
        let wrap = self.wrap_width(v);
        let n = self.bufs[bi].doc.line_count();
        let max_top = n.saturating_sub(self.vis_rows(v) / 2).max(1) - 1;
        let (mut top, mut sub) = (self.tab(v).top, self.tab(v).sub);
        if delta > 0 {
            for _ in 0..delta {
                let r = self.line_rows(bi, top, wrap);
                if sub + 1 < r {
                    sub += 1;
                } else if top < max_top {
                    match self.bufs[bi].next_visible(top) {
                        Some(nx) if nx <= max_top.max(top) || wrap.is_some() => {
                            top = nx;
                            sub = 0;
                        }
                        _ => break,
                    }
                } else {
                    break;
                }
            }
        } else {
            for _ in 0..(-delta) {
                if sub > 0 {
                    sub -= 1;
                } else if let Some(p) = self.bufs[bi].prev_visible(top) {
                    top = p;
                    sub = self.line_rows(bi, p, wrap) - 1;
                } else {
                    break;
                }
            }
        }
        let t = self.tab_mut(v);
        t.top = top;
        t.sub = sub;
    }

    pub fn scroll_x(&mut self, v: usize, dx: f32) {
        if self.wrap_width(v).is_some() {
            return;
        }
        let t = self.tab_mut(v);
        t.x = (t.x + dx).max(0.0);
    }

    /// Scrolls the active view so the primary caret is on screen.
    pub fn reveal_caret(&mut self) {
        let v = self.av;
        if self.views[v].tabs.is_empty() {
            return;
        }
        let bi = self.bi(self.tab(v).buf);
        let caret = self.bufs[bi].caret();
        let line = self.bufs[bi].doc.line_of(caret);
        self.bufs[bi].reveal(line);
        let wrap = self.wrap_width(v);
        let lh = self.gfx.line_h;
        let (cx, cy) = self.pos_xy(bi, line, caret, wrap);
        let csub = (cy / lh).round() as usize;
        let vis = self.vis_rows(v);
        let (top, sub) = (self.tab(v).top, self.tab(v).sub);
        if line < top || (line == top && csub < sub) {
            let t = self.tab_mut(v);
            t.top = line;
            t.sub = csub;
        } else {
            // Is the caret row within `vis` rows of the top?
            let mut l = top;
            let mut seen = 0usize;
            let mut visible = false;
            loop {
                let r = self.line_rows(bi, l, wrap);
                let first = if l == top { sub } else { 0 };
                if l == line {
                    visible = seen + (csub - first.min(csub)) < vis;
                    break;
                }
                seen += r - first;
                if seen >= vis {
                    break;
                }
                match self.bufs[bi].next_visible(l) {
                    Some(n) => l = n,
                    None => break,
                }
            }
            if !visible {
                let (mut tl, mut ts) = (line, csub);
                let mut remaining = vis.saturating_sub(1);
                while remaining > 0 {
                    if ts > 0 {
                        let k = ts.min(remaining);
                        ts -= k;
                        remaining -= k;
                    } else if let Some(p) = self.bufs[bi].prev_visible(tl) {
                        let r = self.line_rows(bi, p, wrap);
                        tl = p;
                        if r <= remaining {
                            remaining -= r;
                            ts = 0;
                        } else {
                            ts = r - remaining;
                            remaining = 0;
                        }
                    } else {
                        break;
                    }
                }
                let t = self.tab_mut(v);
                t.top = tl;
                t.sub = ts;
            }
        }
        if wrap.is_none() {
            let tw = self.text_rect(v).2;
            let cw = self.gfx.char_w;
            let t = self.tab_mut(v);
            if cx < t.x + cw {
                t.x = (cx - tw / 3.0).max(0.0);
            } else if cx > t.x + tw - 3.0 * cw {
                t.x = cx - tw * 2.0 / 3.0;
            }
        } else {
            self.tab_mut(v).x = 0.0;
        }
    }

    pub fn center_caret(&mut self) {
        let v = self.av;
        let line = {
            let b = self.cur_ref();
            b.doc.line_of(b.caret())
        };
        let t = self.tab_mut(v);
        t.top = line;
        t.sub = 0;
        let half = (self.vis_rows(v) / 2) as isize;
        self.scroll_by(v, -half);
    }

    /// Text position under client point (x, y) in view `v`.
    pub fn pos_at(&mut self, v: usize, x: f32, y: f32) -> usize {
        let rows = self.rows(v);
        let bi = self.bi(self.tab(v).buf);
        let (tx, ty, _, _) = self.text_rect(v);
        let lh = self.gfx.line_h;
        let sx = self.tab(v).x;
        let b = &self.bufs[bi];
        if rows.is_empty() {
            return 0;
        }
        if y < ty {
            let r = &rows[0];
            if r.line == 0 && r.y >= ty {
                return 0;
            }
        }
        let row = rows.iter().find(|r| y < r.y + r.rows as f32 * lh).unwrap_or(rows.last().unwrap());
        if y >= row.y + row.rows as f32 * lh && b.next_visible(row.line).is_none() {
            return b.doc.len();
        }
        let Some(l) = self.cache.get(&row.key) else { return b.doc.line_start(row.line) };
        let (mut trailing, mut inside) = (BOOL(0), BOOL(0));
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        let ly = (y - row.y).clamp(0.0, row.rows as f32 * lh - 1.0);
        unsafe {
            let _ = l.layout.HitTestPoint(x - tx + sx, ly, &mut trailing, &mut inside, &mut m);
        }
        let i16 = m.textPosition + if trailing.as_bool() { m.length } else { 0 };
        let rel = l.to_byte(i16);
        (b.doc.line_start(row.line) + rel).min(b.doc.line_end(row.line))
    }

    pub fn view_at(&self, x: f32) -> usize {
        if self.split && x > self.view_rect(1).0 - 1.0 {
            1
        } else {
            0
        }
    }

    /// Scrollbar thumb (y, h) for view `v`.
    pub fn thumb(&self, v: usize) -> Option<(f32, f32)> {
        let (_, ty, _, th) = self.text_rect(v);
        let n = self.buf_of(v).doc.line_count();
        let vis = self.vis_rows(v);
        if n <= 1 && self.tab(v).top == 0 {
            return None;
        }
        let total = (n + vis / 2).max(vis) as f32;
        let h = (th * vis as f32 / total).clamp(28.0, th);
        let y = ty + (th - h) * (self.tab(v).top as f32 / (n.saturating_sub(vis / 2).max(1)) as f32).min(1.0);
        Some((y, h))
    }

    pub fn scroll_to_frac(&mut self, v: usize, f: f32) {
        let n = self.buf_of(v).doc.line_count();
        let vis = self.vis_rows(v);
        let max_top = n.saturating_sub(vis / 2).max(1) - 1;
        let target = ((f.clamp(0.0, 1.0)) * max_top as f32).round() as usize;
        let bi = self.bi(self.tab(v).buf);
        let t = self.bufs[bi].hidden_by(target).unwrap_or(target);
        let tab = self.tab_mut(v);
        tab.top = t;
        tab.sub = 0;
    }
}
