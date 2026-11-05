//! Frame rendering: title bar, tab strips, editors, status bar, overlays.

use crate::app::{App, Hit, Input, PalMode, CAP_W, SCROLL_W, STATUS_H, TAB_H, TITLE_H};
use crate::gfx::{rect, C};
use crate::icons::Icon;

const PANEL_MS: f32 = 170.0;
use crate::settings;
use crate::syntax::LANGS;
use crate::ui::*;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Direct2D::Common::D2D_POINT_2F;
use windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE;
use windows::Win32::Graphics::DirectWrite::{DWRITE_HIT_TEST_METRICS, DWRITE_TEXT_ALIGNMENT_TRAILING};

const D2DERR_RECREATE_TARGET: i32 = 0x8899000Cu32 as i32;

impl App {
    pub fn paint(&mut self) {
        if self.gfx.rt.is_none() || self.views[0].tabs.is_empty() {
            return;
        }
        self.cache_tick += 1;
        self.caret_target.set(None);
        unsafe {
            self.gfx.rt().BeginDraw();
            self.gfx.rt().Clear(Some(&self.gfx.color(C::Bg)));
        }
        self.cmp_sync_views();
        let views = if self.split { 2 } else { 1 };
        for v in 0..views {
            self.paint_view(v);
        }
        if self.split {
            let sx = self.view_rect(1).0 - 1.0;
            self.gfx.fill(sx, TITLE_H, 1.0, self.h - TITLE_H - STATUS_H, C::Border);
        }
        self.paint_title();
        self.paint_status();
        self.paint_find();
        self.paint_palette();
        let r = unsafe { self.gfx.rt().EndDraw(None, None) };
        if let Err(e) = r {
            if e.code().0 == D2DERR_RECREATE_TARGET {
                self.gfx.drop_target();
                self.cache.clear();
            }
        }
        self.sync_caret();
        self.schedule_frame();
    }

    /// Keeps a ~60 fps timer only while something animates.
    fn schedule_frame(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};
        let moving = self.scroll_anim.iter().flatten().any(|t| !t.done())
            || self.tab_ind.borrow().iter().flatten().any(|(x, w, ..)| !x.done() || !w.done())
            || crate::anim::appear(self.pal_since, PANEL_MS) < 1.0 && self.palette.is_some()
            || crate::anim::appear(self.find_since, PANEL_MS) < 1.0 && self.find.is_some();
        if !moving {
            self.scroll_anim = [None, None];
        }
        unsafe {
            if moving && !self.anim_timer {
                SetTimer(self.hwnd, crate::app::T_ANIM, 15, None);
                self.anim_timer = true;
            } else if !moving && self.anim_timer {
                let _ = KillTimer(self.hwnd, crate::app::T_ANIM);
                self.anim_timer = false;
            }
        }
    }

    /// Hover highlights are instant: a fade would redraw the whole window at
    /// 60 fps for every item the mouse crosses.
    fn hover_alpha(&self) -> f32 {
        1.0
    }

    /// The app mark: the pipe "S" on a slate tile, as in the app icon.
    fn draw_logo(&self, x: f32, y: f32, s: f32) {
        let g = &self.gfx;
        let tile = g.tint(0x1C2333, 1.0);
        unsafe {
            let rr = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT { rect: rect(x, y, s, s), radiusX: s * 0.24, radiusY: s * 0.24 };
            g.rt().FillRoundedRectangle(&rr, &tile);
        }
        g.draw_icon_brush(Icon::Pipe, x, y, s, s, &g.tint(0x2F5FC8, 1.0), s * 0.86, s * 0.2);
        g.draw_icon_brush(Icon::Pipe, x, y, s, s, &g.tint(0x7FB6FF, 1.0), s * 0.86, s * 0.12);
    }

    /// Places the Windows caret at the focused element. Windows blinks it,
    /// so an idle window never repaints.
    fn sync_caret(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::{CreateCaret, DestroyCaret, HideCaret, SetCaretPos, ShowCaret};
        let target = if self.focused { self.caret_target.get() } else { None };
        unsafe {
            match target {
                Some((x, y, w, h)) => {
                    let s = self.scale;
                    let size = ((w * s).round().max(1.0) as i32, (h * s).round() as i32);
                    if self.sys_caret != Some(size) {
                        let _ = DestroyCaret();
                        let _ = CreateCaret(self.hwnd, None, size.0, size.1);
                        self.sys_caret = Some(size);
                        self.caret_shown = false;
                    }
                    let _ = SetCaretPos((x * s).round() as i32, (y * s).round() as i32);
                    if !self.caret_shown {
                        let _ = ShowCaret(self.hwnd);
                        self.caret_shown = true;
                    }
                }
                None => {
                    if self.caret_shown {
                        let _ = HideCaret(self.hwnd);
                        self.caret_shown = false;
                    }
                }
            }
        }
    }

    fn paint_title(&self) {
        let g = &self.gfx;
        self.gfx.fill(0.0, 0.0, self.w, TITLE_H, C::Chrome);
        let ha = self.hover_alpha();
        if self.hover == Hit::Menu {
            g.fill_alpha(4.0, 4.0, MENU_W - 8.0, TITLE_H - 8.0, 6.0, C::Hover, ha);
        }
        g.draw_icon(Icon::Menu, 0.0, 0.0, MENU_W, TITLE_H, C::Fg, 16.0, 1.4);
        self.draw_logo(MENU_W + 1.0, 8.0, 18.0);
        let b = self.cur_ref();
        let name = match &b.path {
            Some(p) => p.display().to_string(),
            None => b.name(),
        };
        let title = format!("{name}{}", if b.dirty() { "  \u{2022}" } else { "" });
        let tx = MENU_W + 28.0;
        let tw = self.w - tx - 3.0 * CAP_W - 16.0;
        g.text(&title, g.ui.as_ref().unwrap(), tx, 0.0, tw, TITLE_H, C::Dim);
        if self.recording.is_some() {
            let rw = 120.0;
            g.fill_round(self.w - 3.0 * CAP_W - rw - 10.0, 8.0, rw, 18.0, 9.0, C::Danger);
            g.text("\u{25CF}  Recording macro", g.ui_small.as_ref().unwrap(), self.w - 3.0 * CAP_W - rw, 8.0, rw, 18.0, C::OnAccent);
        }
        for (h, rc) in self.caption_buttons() {
            let hovered = self.hover == h;
            if hovered {
                g.fill_alpha(rc.x, rc.y, rc.w, rc.h, 0.0, if h == Hit::Close { C::Danger } else { C::Hover }, ha);
            }
            let icon = match h {
                Hit::Min => Icon::Minimize,
                Hit::Max if self.maximized => Icon::Restore,
                Hit::Max => Icon::Maximize,
                _ => Icon::Close,
            };
            let c = if hovered && h == Hit::Close { C::OnAccent } else { C::Fg };
            g.draw_icon(icon, rc.x, rc.y, rc.w, rc.h, c, 12.0, 1.1);
        }
        g.fill(0.0, TITLE_H - 1.0, self.w, 1.0, C::Border);
    }

    fn paint_tabs(&self, v: usize) {
        let g = &self.gfx;
        let (vx, vy, vw, _) = self.view_rect(v);
        self.gfx.fill(vx, vy, vw, TAB_H, C::Tabs);
        g.fill(vx, vy + TAB_H - 1.0, vw, 1.0, C::Border);
        let (tabs, plus) = self.tab_layout(v);
        g.clip(vx, vy, vw, TAB_H);
        let focused_view = v == self.av;
        let ha = self.hover_alpha();
        let small = g.ui_small.as_ref().unwrap();
        for (i, t) in tabs.iter().enumerate() {
            let tab = &self.views[v].tabs[i];
            let b = &self.bufs[self.bi(tab.buf)];
            let active = i == self.views[v].active;
            let hovered = matches!(self.hover, Hit::Tab(hv, hi) | Hit::TabClose(hv, hi) if hv == v && hi == i);
            if active {
                g.fill_round(t.x, t.y, t.w, t.h + 6.0, 7.0, C::Bg);
                g.fill(t.x, t.y + t.h - 1.0, t.w, 1.0, C::Bg);
            } else if hovered {
                g.fill_alpha(t.x, t.y + 2.0, t.w, t.h - 6.0, 7.0, C::Hover, ha);
            }
            let mut tx = t.x + 10.0;
            if let Some((label, rgb)) = badge(b) {
                let bw = self.text_w(&label, true) + 10.0;
                let by = t.y + (t.h - 17.0) / 2.0;
                unsafe {
                    let rr = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT { rect: rect(tx, by, bw, 17.0), radiusX: 4.0, radiusY: 4.0 };
                    g.rt().FillRoundedRectangle(&rr, &g.tint(rgb, if g.dark { 0.22 } else { 0.16 }));
                    let u: Vec<u16> = label.encode_utf16().collect();
                    g.rt().DrawText(&u, small, &rect(tx + 5.0, by, bw, 17.0), &g.tint(rgb, 1.0), Default::default(), Default::default());
                }
                tx += bw + 7.0;
            }
            let fg = if active { C::Fg } else { C::Dim };
            let f = if active { g.ui_bold.as_ref() } else { g.ui.as_ref() };
            g.text(&b.name(), f.unwrap(), tx, t.y, t.x + t.w - 30.0 - tx, t.h - 1.0, fg);
            let cr = Self::tab_close_rect(t);
            let close_hover = self.hover == Hit::TabClose(v, i);
            if close_hover {
                g.fill_alpha(cr.x, cr.y, cr.w, cr.h, 4.0, C::Hover, 1.0);
            }
            if b.dirty() && !hovered && Some(b.id) != self.results_buf {
                g.fill_round(cr.x + 6.0, cr.y + 6.0, 8.0, 8.0, 4.0, if active { C::Accent } else { C::Dim });
            } else if active || hovered {
                g.draw_icon(Icon::Close, cr.x, cr.y, cr.w, cr.h, if close_hover { C::Fg } else { C::Dim }, 10.0, 1.2);
            }
        }
        // The active-tab underline glides between tabs.
        if let Some(t) = tabs.get(self.views[v].active) {
            let (tx, tw) = (t.x + 10.0, t.w - 20.0);
            let mut ind = self.tab_ind.borrow_mut();
            let slot = &mut ind[v];
            let restart = match slot {
                Some((_, _, x, w)) => (*x - tx).abs() > 0.5 || (*w - tw).abs() > 0.5,
                None => true,
            };
            if restart {
                let (cx, cw) = slot.as_ref().map(|(x, w, ..)| (x.value(), w.value())).unwrap_or((tx, tw));
                *slot = Some((crate::anim::Tween::new(cx, tx, 220.0), crate::anim::Tween::new(cw, tw, 220.0), tx, tw));
            }
            let (x, w, ..) = slot.as_ref().unwrap();
            g.fill_round(x.value(), t.y, w.value(), 2.5, 1.25, if focused_view { C::Accent } else { C::Faint });
        }
        if self.hover == Hit::NewTab(v) {
            g.fill_alpha(plus.x, plus.y, plus.w, plus.h, 6.0, C::Hover, ha);
        }
        g.draw_icon(Icon::Plus, plus.x, plus.y, plus.w, plus.h, C::Dim, 12.0, 1.3);
        g.unclip();
    }

    fn paint_view(&mut self, v: usize) {
        self.paint_tabs(v);
        if let Some((done, total)) = &self.buf_of(v).loading {
            let (vx, _, vw, _) = self.view_rect(v);
            let (_, ty, _, th) = self.text_rect(v);
            let done = done.load(std::sync::atomic::Ordering::Relaxed);
            let frac = if *total == 0 { 0.0 } else { (done as f32 / *total as f32).min(1.0) };
            let g = &self.gfx;
            let w = (vw - 80.0).min(420.0);
            let x = vx + (vw - w) / 2.0;
            let y = ty + th * 0.38;
            let name = self.buf_of(v).name();
            let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
            let label = if frac >= 1.0 {
                format!("Indexing {name}\u{2026}")
            } else {
                format!("Loading {name}   {:.0} of {:.0} MB", mb(done), mb(*total))
            };
            g.text(&label, g.ui.as_ref().unwrap(), x, y - 30.0, w, 24.0, C::Dim);
            g.fill_round(x, y, w, 6.0, 3.0, C::SelDim);
            g.fill_round(x, y, (w * frac).max(6.0), 6.0, 3.0, C::Accent);
            return;
        }
        if self.buf_of(v).grid.is_some() {
            self.paint_grid(v);
            return;
        }
        let (vx, _, vw, _) = self.view_rect(v);
        let (tx, ty, tw, th) = self.text_rect(v);
        let rows = self.rows(v);
        let bi = self.bi(self.tab(v).buf);
        let wrap = self.wrap_width(v);
        let lh = self.gfx.line_h;
        let sx = self.tab(v).x;
        let gw = self.gutter_w(v);
        let active = v == self.av;
        let show_ws = settings::get().show_ws;
        let (prim, single, n_sels) = {
            let b = &self.bufs[bi];
            (b.primary(), b.sels.len() == 1 && b.primary().is_empty(), b.sels.len())
        };
        let caret_line = self.bufs[bi].doc.line_of(prim.head);

        // Words to mark: find query when the panel is open, else the selected word.
        let marker = match &self.find {
            Some(fb) if active && !fb.fields[0].text.is_empty() => fb.query().compile().ok(),
            _ => {
                let b = &self.bufs[bi];
                if n_sels == 1 && !prim.is_empty() && prim.hi() - prim.lo() < 200 {
                    let t = b.doc.text(prim.lo(), prim.hi());
                    let word = t.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80);
                    if word {
                        crate::search::Query { pat: String::from_utf8_lossy(&t).into_owned(), case: true, word: true, regex: false }.compile().ok()
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        };
        let bracket = if active { crate::edit::match_bracket(&self.bufs[bi], prim.head) } else { None };

        self.gfx.clip(vx, ty, vw, th);
        let mut line_buf = Vec::new();
        for row in &rows {
            let b = &self.bufs[bi];
            let Some(l) = self.cache.get(&row.key) else { continue };
            let ls = b.doc.line_start(row.line);
            let le = b.doc.line_end(row.line);
            let row_h = row.rows as f32 * lh;
            if single && row.line == caret_line {
                self.gfx.fill(vx, row.y, vw - SCROLL_W, row_h, C::CurLine);
            }
            let ox = tx - sx;
            if let Some(d) = &b.diff {
                self.paint_diff_row(d, row.line, vx, row.y, vw - SCROLL_W, row_h);
                let rgb = if d.left { crate::compare::RED } else { crate::compare::GREEN };
                for &(_, a, z) in d.marks_on(row.line) {
                    self.fill_range_tint(l, a as usize, z as usize, ox, row.y, rgb, if self.gfx.dark { 0.38 } else { 0.30 });
                }
            }
            // Selections.
            for s in &b.sels {
                if s.is_empty() || s.hi() < ls || s.lo() > le + 1 {
                    continue;
                }
                let a = s.lo().max(ls) - ls;
                let z = s.hi().min(le) - ls;
                let fill = if active { C::Sel } else { C::SelDim };
                self.fill_range(l, a, z, ox, row.y, fill);
                if s.hi() > le && s.lo() <= le {
                    let (ex, ey) = self.end_xy(l);
                    self.gfx.fill(ox + ex, row.y + ey, self.gfx.char_w * 0.6, lh, fill);
                }
            }
            // Search / same-word marks.
            if let Some(m) = &marker {
                b.doc.gap.copy_range(ls, le, &mut line_buf);
                let text = String::from_utf8_lossy(&line_buf);
                for (a, z) in m.find_all(&text, 500) {
                    if a + ls == prim.lo() && z + ls == prim.hi() {
                        continue;
                    }
                    self.fill_range(l, a, z, ox, row.y, C::Match);
                }
            }
            if let Some((p, q)) = bracket {
                for at in [p, q] {
                    if at >= ls && at < le {
                        let (x0, y0) = self.xy16(l, l.to16(at - ls));
                        let (x1, _) = self.xy16(l, l.to16(at + 1 - ls));
                        self.gfx.stroke_round(ox + x0 - 0.5, row.y + y0, x1 - x0 + 1.0, lh, 2.0, C::Dim, 1.0);
                    }
                }
            }
            unsafe {
                self.gfx.rt().DrawTextLayout(D2D_POINT_2F { x: ox, y: row.y }, &l.layout, self.gfx.b(C::Fg), D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            if show_ws {
                b.doc.gap.copy_range(ls, le, &mut line_buf);
                for (k, &c) in line_buf.iter().enumerate() {
                    if c == b'\t' {
                        let (x0, y0) = self.xy16(l, l.to16(k));
                        let (x1, _) = self.xy16(l, l.to16(k + 1));
                        let my = row.y + y0 + lh / 2.0;
                        self.gfx.line(ox + x0 + 2.0, my, ox + x1.max(x0 + 6.0) - 3.0, my, C::Faint, 1.0);
                    }
                }
                if row.line + 1 < b.doc.line_count() {
                    let (ex, ey) = self.end_xy(l);
                    self.gfx.text("\u{AC}", self.gfx.mono.as_ref().unwrap(), ox + ex + 1.0, row.y + ey, 20.0, lh, C::Faint);
                }
            }
            if b.is_folded(row.line) {
                let (ex, ey) = self.end_xy(l);
                let fx = ox + ex + 8.0;
                self.gfx.fill_round(fx, row.y + ey + 2.0, 30.0, lh - 4.0, 4.0, C::SelDim);
                self.gfx.text("\u{22EF}", self.gfx.ui.as_ref().unwrap(), fx + 8.0, row.y + ey, 30.0, lh, C::Dim);
            }
        }
        // Carets: the primary one is the system caret, extra ones are drawn.
        if active && self.palette.is_none() && !self.in_find {
            let b = &self.bufs[bi];
            let n = b.sels.len();
            let carets: Vec<(usize, usize)> = b.sels.iter().map(|s| (b.doc.line_of(s.head), s.head)).collect();
            for (k, (line, pos)) in carets.into_iter().enumerate() {
                let Some(row) = rows.iter().find(|r| r.line == line) else { continue };
                let (x, y) = self.pos_xy(bi, line, pos, wrap);
                let cx = tx - sx + x;
                let cy = row.y + y;
                if cx < tx - 1.0 || cx > tx + tw || cy + lh <= ty || cy >= ty + th {
                    continue;
                }
                if k + 1 == n {
                    let cell = self.gfx.char_w.max(2.0);
                    self.caret_target.set(Some(if self.overtype { (cx, cy + lh - 3.0, cell, 2.0) } else { (cx - 0.5, cy + 1.0, 2.0, lh - 2.0) }));
                } else {
                    self.gfx.fill(cx - 0.5, cy + 1.0, 2.0, lh - 2.0, C::Accent);
                }
            }
        }
        self.gfx.unclip();

        // Gutter.
        let g = &self.gfx;
        g.clip(vx, ty, gw, th);
        g.fill(vx, ty, gw, th, C::Bg);
        let b = &self.bufs[bi];
        let num_fmt = g.mono_num.as_ref().unwrap();
        let gutter_hover = self.hover == Hit::Gutter(v);
        for row in &rows {
            if single && row.line == caret_line {
                g.fill(vx, row.y, gw, row.rows as f32 * lh, C::CurLine);
            }
            if b.bookmarks.binary_search(&row.line).is_ok() {
                g.fill_round(vx + 5.0, row.y + lh / 2.0 - 4.5, 9.0, 9.0, 4.5, C::Accent);
            }
            // Compare sides number lines as in their files; filler has none.
            let num = match &b.diff {
                Some(d) => {
                    self.paint_diff_gutter(d, row.line, vx, row.y, row.rows as f32 * lh);
                    match d.nums.get(row.line).copied().unwrap_or(0) {
                        0 => String::new(),
                        n => n.to_string(),
                    }
                }
                None => (row.line + 1).to_string(),
            };
            let fg = if row.line == caret_line { C::Fg } else { C::Faint };
            g.text(&num, num_fmt, vx + 14.0, row.y, gw - 14.0 - 22.0, lh, fg);
            let folded = b.is_folded(row.line);
            if folded || (gutter_hover && b.fold_end(row.line).is_some()) {
                let icon = if folded { Icon::ChevronRight } else { Icon::ChevronDown };
                g.draw_icon(icon, vx + gw - 20.0, row.y, 16.0, lh, if folded { C::Accent } else { C::Dim }, 11.0, 1.3);
            }
        }
        g.unclip();

        // Scrollbar.
        if let Some((y, h)) = self.thumb(v) {
            let x = vx + vw - SCROLL_W;
            let hot = matches!(self.hover, Hit::VScroll(hv) if hv == v) || matches!(self.drag, crate::app::Drag::Scroll { view, .. } if view == v);
            g.fill_round(x + 3.0, y + 2.0, SCROLL_W - 6.0, h - 4.0, 3.0, if hot { C::Dim } else { C::Faint });
            // Bookmark ticks along the track.
            let n = b.doc.line_count().max(1) as f32;
            for &bm in &b.bookmarks {
                let by = ty + th * bm as f32 / n;
                g.fill(x + 2.0, by, SCROLL_W - 4.0, 2.0, C::Accent);
            }
        }
        if let Some(d) = &b.diff {
            self.paint_diff_ticks(d, vx + vw - SCROLL_W, ty, th);
        }
        let _ = wrap;
    }

    fn fill_range(&self, l: &crate::app::LineLayout, a: usize, z: usize, ox: f32, oy: f32, c: C) {
        for (x, y, w) in self.range_rects(l, a, z) {
            self.gfx.fill(ox + x, oy + y, w.max(2.0), self.gfx.line_h, c);
        }
    }

    fn fill_range_tint(&self, l: &crate::app::LineLayout, a: usize, z: usize, ox: f32, oy: f32, rgb: u32, alpha: f32) {
        for (x, y, w) in self.range_rects(l, a, z) {
            self.gfx.fill_tint(ox + x, oy + y, w.max(2.0), self.gfx.line_h, rgb, alpha);
        }
    }

    /// Layout-relative rectangles (x, y, width) covering bytes a..z of a line.
    fn range_rects(&self, l: &crate::app::LineLayout, a: usize, z: usize) -> Vec<(f32, f32, f32)> {
        let (a16, z16) = (l.to16(a), l.to16(z));
        if z16 <= a16 {
            return Vec::new();
        }
        let mut m = [DWRITE_HIT_TEST_METRICS::default(); 16];
        let mut n = 0u32;
        unsafe {
            if l.layout.HitTestTextRange(a16, z16 - a16, 0.0, 0.0, Some(&mut m), &mut n).is_err() {
                return Vec::new();
            }
        }
        m[..(n as usize).min(16)].iter().map(|r| (r.left, r.top, r.width)).collect()
    }

    fn xy16(&self, l: &crate::app::LineLayout, i16: u32) -> (f32, f32) {
        let (mut x, mut y) = (0.0, 0.0);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = l.layout.HitTestTextPosition(i16, BOOL(0), &mut x, &mut y, &mut m);
        }
        (x, y)
    }

    /// End of a layout's last row.
    fn end_xy(&self, l: &crate::app::LineLayout) -> (f32, f32) {
        let n = (l.map.len() - 1) as u32;
        self.xy16(l, n)
    }

    fn paint_status(&mut self) {
        let y = self.h - STATUS_H;
        let mut segs: Vec<(String, u8)> = Vec::new();
        let grid = if self.grid_active() { Some(self.grid_status()) } else { self.cmp_status() };
        {
            let b = self.cur_ref();
            let p = b.caret();
            let (l, _) = b.line_col(p);
            let col = b.display_col(p) + 1;
            let selected: usize = b.sels.iter().map(|s| s.hi() - s.lo()).sum();
            let mut pos = format!("Ln {}, Col {}", l + 1, col);
            if b.sels.len() > 1 {
                pos += &format!("  ({} carets)", b.sels.len());
            } else if selected > 0 {
                pos += &format!("  ({selected} selected)");
            }
            if let Some((_, gp)) = &grid {
                pos = gp.clone();
            }
            let s = settings::get();
            if let Some(m) = &self.upd.available {
                segs.push((format!("Update {}", m.version), 9));
            }
            if self.recording.is_some() {
                segs.push(("\u{25CF} REC".into(), 8));
            }
            if self.overtype {
                segs.push(("OVR".into(), 7));
            }
            if s.wrap {
                segs.push(("Wrap".into(), 6));
            }
            if s.zoom != 0 {
                let pct = ((s.font_size + s.zoom as f32) / s.font_size * 100.0).round();
                segs.push((format!("{pct}%"), 5));
            }
            // Grid / text switch for table files, and the delimiter in the grid.
            if let Some(g) = &b.grid {
                segs.push(("Grid".into(), 10));
                segs.push((crate::csv::delim_name(g.delim).into(), 11));
            } else if b.path.as_deref().map_or(false, crate::csv::is_table_path) {
                segs.push(("Text".into(), 10));
            }
            segs.push((pos, 0));
            if grid.is_none() {
                segs.push((format!("{}: {}", if s.use_tabs { "Tabs" } else { "Spaces" }, s.tab_size), 1));
            }
            // Encoding and line endings belong to the files, not to a compare.
            if b.diff.is_none() {
                segs.push((b.enc.name().to_string(), 2));
                segs.push((format!("{}{}", b.eol.name(), if b.mixed_eol { " (mixed)" } else { "" }), 3));
            }
            segs.push((LANGS[b.lang].name.to_string(), 4));
        }
        let g = &self.gfx;
        self.gfx.fill(0.0, y, self.w, STATUS_H, C::Chrome);
        g.fill(0.0, y, self.w, 1.0, C::Border);
        let small = g.ui_small.as_ref().unwrap();
        let mut x = self.w - 8.0;
        let mut hits = Vec::new();
        for (text, id) in segs.iter().rev() {
            // The update button carries an arrow icon before its label.
            let icon_w = if *id == 9 { 16.0 } else { 0.0 };
            let w = self.text_w(text, true) + 18.0 + icon_w;
            x -= w;
            if *id == 9 {
                g.fill_round(x + 3.0, y + 3.0, w - 6.0, STATUS_H - 6.0, 4.0, C::Accent);
                g.draw_icon(Icon::Update, x + 6.0, y, 14.0, STATUS_H, C::OnAccent, 11.0, 1.4);
            } else if self.hover == Hit::Status(*id as usize) {
                g.fill_alpha(x, y + 1.0, w, STATUS_H - 1.0, 0.0, C::Hover, self.hover_alpha());
            }
            let c = match *id {
                8 => C::Danger,
                9 => C::OnAccent,
                _ => C::Dim,
            };
            g.text(text, small, x + 9.0 + icon_w, y, w, STATUS_H, c);
            hits.push((x, x + w, *id));
        }
        let left = match (&self.status, grid) {
            (Some(s), _) => s.clone(),
            (None, Some((summary, _))) => summary,
            (None, None) => {
                let b = self.cur_ref();
                let n = b.doc.line_count();
                format!("{} lines  \u{B7}  {} bytes", n, b.doc.len())
            }
        };
        g.text(&left, small, 12.0, y, (x - 24.0).max(0.0), STATUS_H, C::Dim);
        self.status_segs = hits;
    }

    pub(crate) fn draw_input(&self, inp: &Input, rc: &Rect, focused: bool, placeholder: &str, right_pad: f32) {
        let g = &self.gfx;
        g.fill_round(rc.x, rc.y, rc.w, rc.h, 5.0, C::Bg);
        g.stroke_round(rc.x, rc.y, rc.w, rc.h, 5.0, if focused { C::Accent } else { C::Border }, 1.0);
        let f = g.ui.as_ref().unwrap();
        let tx = rc.x + 8.0;
        let tw = rc.w - 16.0 - right_pad;
        if inp.text.is_empty() {
            g.text(placeholder, f, tx, rc.y, tw, rc.h, C::Faint);
        }
        let u: Vec<u16> = inp.text.encode_utf16().collect();
        let Ok(lay) = (unsafe { g.dw.CreateTextLayout(&u, f, 10000.0, rc.h) }) else { return };
        let to16 = |b: usize| inp.text[..b].encode_utf16().count() as u32;
        let pos = |i: u32| {
            let (mut x, mut y) = (0.0, 0.0);
            let mut m = DWRITE_HIT_TEST_METRICS::default();
            unsafe {
                let _ = lay.HitTestTextPosition(i, BOOL(0), &mut x, &mut y, &mut m);
            }
            x
        };
        let cx = pos(to16(inp.caret));
        let shift = (cx - tw + 4.0).max(0.0);
        g.clip(tx, rc.y, tw, rc.h);
        let (a, z) = inp.sel();
        if focused && a != z {
            let (x0, x1) = (pos(to16(a)), pos(to16(z)));
            g.fill(tx + x0 - shift, rc.y + 4.0, x1 - x0, rc.h - 8.0, C::Sel);
        }
        unsafe {
            g.rt().DrawTextLayout(D2D_POINT_2F { x: tx - shift, y: rc.y }, &lay, g.b(C::Fg), D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        if focused {
            self.caret_target.set(Some((tx + cx - shift, rc.y + 6.0, 2.0, rc.h - 12.0)));
        }
        g.unclip();
    }

    fn panel(&self, rc: &Rect) {
        let g = &self.gfx;
        for (i, c) in [C::Chrome, C::Border].iter().enumerate() {
            let d = 3.0 - i as f32 * 2.0;
            g.fill_round(rc.x - d + 1.0, rc.y - d + 3.0, rc.w + 2.0 * d - 2.0, rc.h + 2.0 * d - 2.0, 9.0, *c);
        }
        g.fill_round(rc.x, rc.y, rc.w, rc.h, 8.0, C::Popup);
        g.stroke_round(rc.x, rc.y, rc.w, rc.h, 8.0, C::Border, 1.0);
    }

    /// Draws `f` fading in and sliding down while the panel's appear
    /// animation (started at `since`) runs.
    fn panel_fx(&self, since: Option<std::time::Instant>, f: impl FnOnce(&Self)) {
        use std::mem::ManuallyDrop;
        use windows::Foundation::Numerics::Matrix3x2;
        use windows::Win32::Graphics::Direct2D::{D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_LAYER_OPTIONS_NONE, D2D1_LAYER_PARAMETERS};
        let t = crate::anim::appear(since, PANEL_MS);
        if t >= 1.0 {
            f(self);
            return;
        }
        let rt = self.gfx.rt();
        unsafe {
            let Ok(layer) = rt.CreateLayer(None) else {
                f(self);
                return;
            };
            let ident = Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: 0.0, M32: 0.0 };
            let params = D2D1_LAYER_PARAMETERS {
                contentBounds: rect(-1e6, -1e6, 2e6, 2e6),
                geometricMask: ManuallyDrop::new(None),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: ident,
                opacity: t,
                opacityBrush: ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS_NONE,
            };
            rt.PushLayer(&params, &layer);
            rt.SetTransform(&Matrix3x2 { M32: (1.0 - t) * -10.0, ..ident });
            f(self);
            rt.SetTransform(&ident);
            rt.PopLayer();
        }
    }

    fn paint_find(&self) {
        self.panel_fx(self.find_since, |s| s.paint_find_inner());
    }

    fn paint_find_inner(&self) {
        let Some((panel, fields, btns)) = self.find_layout() else { return };
        let fb = self.find.as_ref().unwrap();
        let g = &self.gfx;
        self.panel(&panel);
        let ui = g.ui.as_ref().unwrap();
        let small = g.ui_small.as_ref().unwrap();
        for (id, rc) in &fields {
            let ph = match id {
                0 => "Find",
                1 => "Replace",
                2 => "Folder",
                _ => "Filters, e.g. *.rs;*.txt",
            };
            let pad = if *id == 0 { 76.0 } else { 0.0 };
            self.draw_input(&fb.fields[*id], rc, self.in_find && fb.focus == *id, ph, pad);
            if *id == 0 && !fb.fif {
                let label = match (&fb.err, fb.count) {
                    (Some(_), _) => "Bad regex".to_string(),
                    (None, Some(0)) => "No results".to_string(),
                    (None, Some(n)) => format!("{n} found"),
                    (None, None) => String::new(),
                };
                let c = if fb.err.is_some() || fb.count == Some(0) { C::Danger } else { C::Dim };
                g.text(&label, small, rc.x + rc.w + 8.0, rc.y, 64.0, rc.h, c);
            }
        }
        for (id, rc) in &btns {
            let hovered = self.hover == Hit::FindBtn(*id);
            let on = match *id {
                FB_CASE => fb.case,
                FB_WORD => fb.word,
                FB_REGEX => fb.regex,
                _ => false,
            };
            if on {
                g.fill_round(rc.x, rc.y, rc.w, rc.h, 4.0, C::SelDim);
                g.stroke_round(rc.x, rc.y, rc.w, rc.h, 4.0, C::Accent, 1.0);
            } else if hovered {
                g.fill_alpha(rc.x, rc.y, rc.w, rc.h, 4.0, C::Hover, self.hover_alpha());
            }
            let fg = if on { C::Accent } else { C::Fg };
            match *id {
                FB_CASE => g.text("Aa", small, rc.x + 3.0, rc.y, rc.w, rc.h, fg),
                FB_WORD => g.text("ab", small, rc.x + 4.0, rc.y, rc.w, rc.h, fg),
                FB_REGEX => g.text(".*", ui, rc.x + 6.0, rc.y, rc.w, rc.h, fg),
                FB_PREV => g.draw_icon(Icon::ChevronUp, rc.x, rc.y, rc.w, rc.h, C::Fg, 12.0, 1.3),
                FB_NEXT => g.draw_icon(Icon::ChevronDown, rc.x, rc.y, rc.w, rc.h, C::Fg, 12.0, 1.3),
                FB_CLOSE => g.draw_icon(Icon::Close, rc.x, rc.y, rc.w, rc.h, C::Fg, 10.0, 1.3),
                FB_MODE => g.draw_icon(if fb.replace { Icon::ChevronDown } else { Icon::ChevronRight }, rc.x, rc.y, rc.w, rc.h, C::Dim, 11.0, 1.3),
                FB_REPL1 | FB_REPLALL | FB_SEARCH => {
                    if !hovered {
                        g.stroke_round(rc.x, rc.y, rc.w, rc.h, 4.0, C::Border, 1.0);
                    }
                    let label = match *id {
                        FB_REPL1 => "Replace",
                        FB_REPLALL => "All",
                        _ => "Search",
                    };
                    let w = g.measure(label, small);
                    g.text(label, small, rc.x + (rc.w - w) / 2.0, rc.y, rc.w, rc.h, C::Fg);
                }
                _ => {}
            }
        }
    }

    fn paint_palette(&self) {
        self.panel_fx(self.pal_since, |s| s.paint_palette_inner());
    }

    fn paint_palette_inner(&self) {
        let Some((pr, rows)) = self.palette_rect() else { return };
        let p = self.palette.as_ref().unwrap();
        let g = &self.gfx;
        self.panel(&pr);
        g.draw_icon(Icon::Search, pr.x + 6.0, pr.y + 6.0, 28.0, PAL_INPUT - 12.0, C::Dim, 14.0, 1.4);
        let inp = r(pr.x + 34.0, pr.y + 6.0, pr.w - 42.0, PAL_INPUT - 12.0);
        let ph = if p.title.is_empty() { "Type a command\u{2026}".to_string() } else { p.title.clone() };
        self.draw_input(&p.input, &inp, true, &ph, 0.0);
        let ui = g.ui.as_ref().unwrap();
        let small = g.ui_small.as_ref().unwrap();
        if p.mode == PalMode::Goto {
            g.text(&p.title, small, pr.x + 16.0, pr.y + PAL_INPUT + 5.0, pr.w - 32.0, PAL_ROW, C::Dim);
            return;
        }
        for k in 0..rows {
            let i = p.scroll + k;
            let Some(&item) = p.shown.get(i) else { break };
            let it = &p.items[item];
            let y = pr.y + PAL_INPUT + 5.0 + k as f32 * PAL_ROW;
            if i == p.sel {
                g.fill_round(pr.x + 6.0, y, pr.w - 12.0, PAL_ROW, 5.0, C::SelDim);
                g.fill_round(pr.x + 6.0, y + 7.0, 3.0, PAL_ROW - 14.0, 1.5, C::Accent);
            } else if self.hover == Hit::PalRow(k) {
                g.fill_alpha(pr.x + 6.0, y, pr.w - 12.0, PAL_ROW, 5.0, C::Hover, self.hover_alpha());
            }
            let hw = if it.hint.is_empty() { 0.0 } else { (g.measure(&it.hint, small) + 8.0).min(pr.w * 0.45) };
            g.text(&it.label, ui, pr.x + 18.0, y, pr.w - 36.0 - hw, PAL_ROW, C::Fg);
            if !it.hint.is_empty() {
                let f = small;
                unsafe {
                    let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
                }
                g.text(&it.hint, f, pr.x + pr.w - 16.0 - hw, y, hw, PAL_ROW, C::Dim);
                unsafe {
                    let _ = f.SetTextAlignment(Default::default());
                }
            }
        }
        if p.shown.is_empty() {
            g.text("No matching commands", small, pr.x + 18.0, pr.y + PAL_INPUT, pr.w - 36.0, PAL_ROW, C::Faint);
        }
        let _ = rect;
    }
}
