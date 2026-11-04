//! Keyboard and mouse handling.

use crate::app::{Action, App, Cmd, Drag, Hit, PalMode, TITLE_H};
use crate::buffer::{EditKind, Sel};
use crate::edit::{self, Move};
use crate::sys;
use crate::ui::*;
use std::time::Instant;
use windows::Win32::Foundation::{LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, WM_CLOSE};

const VK_BACK: u32 = 0x08;
const VK_TAB: u32 = 0x09;
const VK_RETURN: u32 = 0x0D;
const VK_ESCAPE: u32 = 0x1B;
const VK_PRIOR: u32 = 0x21;
const VK_NEXT: u32 = 0x22;
const VK_END: u32 = 0x23;
const VK_HOME: u32 = 0x24;
const VK_LEFT: u32 = 0x25;
const VK_UP: u32 = 0x26;
const VK_RIGHT: u32 = 0x27;
const VK_DOWN: u32 = 0x28;
const VK_INSERT: u32 = 0x2D;
const VK_DELETE: u32 = 0x2E;
const VK_F1: u32 = 0x70;

fn key_cmd(vk: u32, ctrl: bool, shift: bool, alt: bool) -> Option<Cmd> {
    let k = |c: char| c as u32;
    Some(match (ctrl, shift, alt, vk) {
        (true, false, false, v) if v == k('N') => Cmd::New,
        (true, false, false, v) if v == k('O') => Cmd::Open,
        (true, true, false, v) if v == k('O') => Cmd::Recent,
        (true, false, false, v) if v == k('S') => Cmd::Save,
        (true, false, true, v) if v == k('S') => Cmd::SaveAs,
        (true, true, false, v) if v == k('S') => Cmd::SaveAll,
        (true, false, false, v) if v == k('W') || v == 0x73 => Cmd::Close,
        (true, true, false, v) if v == k('T') => Cmd::ReopenClosed,
        (true, false, false, v) if v == k('P') => Cmd::SwitchFile,
        (true, true, false, v) if v == k('P') => Cmd::Palette,
        (false, false, false, VK_F1) => Cmd::Palette,
        (true, false, false, v) if v == k('Z') => Cmd::Undo,
        (true, true, false, v) if v == k('Z') => Cmd::Redo,
        (true, false, false, v) if v == k('Y') => Cmd::Redo,
        (true, false, false, v) if v == k('X') => Cmd::Cut,
        (true, false, false, v) if v == k('C') || v == VK_INSERT => Cmd::Copy,
        (true, false, false, v) if v == k('V') => Cmd::Paste,
        (false, true, false, VK_INSERT) => Cmd::Paste,
        (false, true, false, VK_DELETE) => Cmd::Cut,
        (true, false, false, v) if v == k('A') => Cmd::SelectAll,
        (true, false, true, v) if v == k('L') => Cmd::SelectLine,
        (true, false, false, v) if v == k('E') => Cmd::AddNext,
        (true, false, true, VK_UP) => Cmd::ColumnUp,
        (true, false, true, VK_DOWN) => Cmd::ColumnDown,
        (true, false, false, v) if v == k('F') => Cmd::Find,
        (true, false, false, v) if v == k('H') => Cmd::Replace,
        (true, true, false, v) if v == k('F') => Cmd::FindInFiles,
        (false, false, false, 0x72) => Cmd::FindNext,
        (false, true, false, 0x72) => Cmd::FindPrev,
        (true, false, false, v) if v == k('G') => Cmd::GotoLine,
        (true, false, false, v) if v == k('B') => Cmd::GotoBrace,
        (true, false, false, v) if v == k('Q') || v == 0xBF => Cmd::ToggleComment,
        (true, false, false, v) if v == k('K') => Cmd::Comment,
        (true, true, false, v) if v == k('K') => Cmd::Uncomment,
        (true, false, false, v) if v == k('D') => Cmd::Duplicate,
        (true, true, false, v) if v == k('L') => Cmd::DeleteLine,
        (true, true, false, VK_UP) => Cmd::MoveUp,
        (true, true, false, VK_DOWN) => Cmd::MoveDown,
        (true, false, false, v) if v == k('J') => Cmd::Join,
        (true, true, false, v) if v == k('U') => Cmd::Upper,
        (true, false, false, v) if v == k('U') => Cmd::Lower,
        (true, false, false, 0x71) => Cmd::ToggleBookmark,
        (false, false, false, 0x71) => Cmd::NextBookmark,
        (false, true, false, 0x71) => Cmd::PrevBookmark,
        (true, false, true, v) if v == k('F') => Cmd::ToggleFold,
        (false, false, true, 0x30) => Cmd::FoldAll,
        (false, true, true, 0x30) => Cmd::UnfoldAll,
        (true, _, false, 0xBB) | (true, _, false, 0x6B) => Cmd::ZoomIn,
        (true, _, false, 0xBD) | (true, _, false, 0x6D) => Cmd::ZoomOut,
        (true, false, false, 0x30) | (true, false, false, 0x60) => Cmd::ZoomReset,
        (false, false, true, v) if v == k('Z') => Cmd::Wrap,
        (false, false, false, 0x77) => Cmd::FocusOther,
        (true, false, false, VK_TAB) | (true, false, false, VK_NEXT) => Cmd::NextTab,
        (true, true, false, VK_TAB) | (true, false, false, VK_PRIOR) => Cmd::PrevTab,
        (true, true, false, v) if v == k('R') => Cmd::MacroRecord,
        (true, true, false, v) if v == k('M') => Cmd::MacroPlay,
        (false, false, false, VK_INSERT) => Cmd::Overtype,
        _ => return None,
    })
}

impl App {
    /// Returns true when the key was consumed.
    pub fn on_key(&mut self, vk: u32, ctrl: bool, shift: bool, alt: bool) -> bool {
        if matches!(vk, 0x10 | 0x11 | 0x12) {
            return false;
        }
        self.touch();
        if self.palette.is_some() {
            return self.palette_key(vk, ctrl, shift);
        }
        if self.in_find && self.find.is_some() && self.find_key(vk, ctrl, shift, alt) {
            return true;
        }
        if let Some(c) = key_cmd(vk, ctrl, shift, alt) {
            if c != Cmd::MacroRecord && c != Cmd::MacroPlay {
                self.record(Action::Key(vk, ctrl, shift, alt));
            }
            self.exec(c);
            return true;
        }
        let handled = self.editor_key(vk, ctrl, shift, alt);
        if handled {
            self.record(Action::Key(vk, ctrl, shift, alt));
        }
        handled
    }

    fn record(&mut self, a: Action) {
        if !self.replaying {
            if let Some(r) = self.recording.as_mut() {
                r.push(a);
            }
        }
    }

    fn editor_key(&mut self, vk: u32, ctrl: bool, shift: bool, alt: bool) -> bool {
        if self.cur_ref().loading.is_some() {
            return false;
        }
        let vis = self.vis_rows(self.av);
        let column = alt && shift && !ctrl;
        let mv = match vk {
            VK_LEFT => Some(if ctrl { Move::WordLeft } else { Move::Left }),
            VK_RIGHT => Some(if ctrl { Move::WordRight } else { Move::Right }),
            VK_UP if ctrl && !shift => {
                self.scroll_smooth(self.av, -1);
                self.redraw();
                return true;
            }
            VK_DOWN if ctrl && !shift => {
                self.scroll_smooth(self.av, 1);
                self.redraw();
                return true;
            }
            VK_UP => Some(Move::Up(1)),
            VK_DOWN => Some(Move::Down(1)),
            VK_HOME => Some(if ctrl { Move::DocStart } else { Move::Home }),
            VK_END => Some(if ctrl { Move::DocEnd } else { Move::End }),
            VK_PRIOR => Some(Move::Up(vis.saturating_sub(1).max(1))),
            VK_NEXT => Some(Move::Down(vis.saturating_sub(1).max(1))),
            _ => None,
        };
        if let Some(m) = mv {
            if alt && !column {
                return false;
            }
            let b = self.cur();
            if column {
                // Alt+Shift+arrows grow a rectangular selection.
                let p = b.primary();
                let anchor = b.sels.first().map(|s| s.anchor).unwrap_or(p.anchor);
                let (al, ac) = (b.doc.line_of(anchor), b.display_col(anchor));
                let (mut hl, mut hc) = (b.doc.line_of(p.head), b.want_col.unwrap_or_else(|| b.display_col(p.head)));
                match m {
                    Move::Left => hc = hc.saturating_sub(1),
                    Move::Right => hc += 1,
                    Move::Up(n) => hl = hl.saturating_sub(n),
                    Move::Down(n) => hl = (hl + n).min(b.doc.line_count() - 1),
                    _ => {}
                }
                edit::column_select_cols(b, al, ac, hl, hc);
                b.want_col = Some(hc);
            } else {
                // Page moves also scroll the view by a page.
                if matches!(vk, VK_PRIOR | VK_NEXT) {
                    let d = vis.saturating_sub(1).max(1) as isize;
                    let av = self.av;
                    self.scroll_smooth(av, if vk == VK_PRIOR { -d } else { d });
                }
                let b = self.cur();
                edit::move_carets(b, m, shift);
            }
            self.after_move();
            return true;
        }
        let b = self.cur();
        match vk {
            VK_BACK if ctrl => edit::delete_word(b, false),
            VK_DELETE if ctrl => edit::delete_word(b, true),
            VK_BACK if !alt => edit::backspace(b),
            VK_DELETE if !alt && !shift => edit::delete(b),
            VK_RETURN if !alt && !ctrl => {
                if self.follow_result() {
                    return true;
                }
                edit::newline(self.cur());
            }
            VK_TAB if !ctrl && !alt => edit::indent(b, shift),
            VK_ESCAPE => {
                if self.find.is_some() {
                    self.find = None;
                    self.in_find = false;
                    self.redraw();
                    return true;
                }
                let b = self.cur();
                if b.sels.len() > 1 {
                    let p = b.primary();
                    b.sels = vec![p];
                } else {
                    let h = b.caret();
                    b.sels = vec![Sel::caret(h)];
                }
                self.redraw();
                return true;
            }
            _ => return false,
        }
        self.after_edit();
        true
    }

    pub fn after_move(&mut self) {
        self.reveal_caret();
        self.redraw();
    }

    pub fn after_edit(&mut self) {
        self.edited();
        self.reveal_caret();
        self.recount_if_open();
        self.redraw();
    }

    pub fn on_char(&mut self, ch: char) {
        if (ch as u32) < 0x20 || ch as u32 == 0x7F {
            return;
        }
        self.touch();
        if let Some(p) = self.palette.as_mut() {
            p.input.insert(&ch.to_string());
            self.filter_palette();
            self.redraw();
            return;
        }
        if self.in_find {
            if let Some(fb) = self.find.as_mut() {
                let f = fb.focus;
                fb.fields[f].insert(&ch.to_string());
                if f == 0 {
                    self.incremental_find();
                }
                self.redraw();
                return;
            }
        }
        if self.cur_ref().loading.is_some() {
            return;
        }
        self.record(Action::Char(ch));
        let mut s = [0u8; 4];
        let bytes = ch.encode_utf8(&mut s).as_bytes().to_vec();
        let over = self.overtype;
        let b = self.cur();
        if over && b.sels.iter().all(|s| s.is_empty()) {
            let ch: Vec<_> = b
                .sorted_sels()
                .iter()
                .map(|s| {
                    let e = if s.head < b.doc.line_end(b.doc.line_of(s.head)) { b.doc.next_char(s.head) } else { s.head };
                    (s.head, e, bytes.clone())
                })
                .collect();
            b.apply(ch, EditKind::Typing);
        } else {
            edit::insert(b, &bytes, EditKind::Typing);
        }
        self.after_edit();
    }

    fn incremental_find(&mut self) {
        self.recount();
        let Some(fb) = &self.find else { return };
        let q = fb.query();
        if fb.fif {
            return;
        }
        let Ok(m) = q.compile() else { return };
        let b = self.cur();
        let from = b.primary().lo();
        let hit = {
            let text = String::from_utf8_lossy(b.doc.gap.contiguous());
            m.find_from(&text, from).or_else(|| m.find_from(&text, 0))
        };
        if let Some((a, z)) = hit {
            b.sels = vec![Sel { anchor: a, head: z }];
            self.reveal_caret();
        }
    }

    fn palette_key(&mut self, vk: u32, ctrl: bool, shift: bool) -> bool {
        let hwnd = self.hwnd;
        let Some(p) = self.palette.as_mut() else { return false };
        let n = p.shown.len();
        match vk {
            VK_ESCAPE => self.palette = None,
            VK_RETURN => self.palette_accept(),
            VK_UP if n > 0 => p.sel = (p.sel + n - 1) % n,
            VK_DOWN if n > 0 => p.sel = (p.sel + 1) % n,
            VK_PRIOR => p.sel = p.sel.saturating_sub(PAL_MAX),
            VK_NEXT => p.sel = (p.sel + PAL_MAX).min(n.saturating_sub(1)),
            0x50 if ctrl && shift => self.palette = None,
            _ => {
                if p.input.key(vk, ctrl, shift, hwnd) {
                    self.filter_palette();
                }
            }
        }
        if let Some(p) = self.palette.as_mut() {
            if p.sel < p.scroll {
                p.scroll = p.sel;
            } else if p.sel >= p.scroll + PAL_MAX {
                p.scroll = p.sel + 1 - PAL_MAX;
            }
            if p.mode == PalMode::Goto {
                p.shown.clear();
            }
        }
        self.redraw();
        true
    }

    fn find_key(&mut self, vk: u32, ctrl: bool, shift: bool, alt: bool) -> bool {
        let hwnd = self.hwnd;
        let fb = self.find.as_mut().unwrap();
        let fields = fb.visible_fields();
        match (vk, ctrl, alt) {
            (VK_ESCAPE, _, _) => {
                self.find = None;
                self.in_find = false;
            }
            (VK_RETURN, false, false) => {
                if fb.fif {
                    self.run_fif();
                } else if fb.focus == 1 {
                    self.replace_one();
                } else {
                    self.find_step(!shift);
                }
            }
            (VK_RETURN, true, true) | (VK_RETURN, false, true) => {
                if !fb.fif && fb.replace {
                    self.replace_all();
                }
            }
            (VK_TAB, false, false) => {
                let i = fields.iter().position(|&f| f == fb.focus).unwrap_or(0);
                let n = fields.len();
                fb.focus = fields[if shift { (i + n - 1) % n } else { (i + 1) % n }];
                let f = fb.focus;
                fb.fields[f].anchor = 0;
                fb.fields[f].caret = fb.fields[f].text.len();
            }
            (0x43, false, true) => {
                fb.case = !fb.case;
                self.recount();
            }
            (0x57, false, true) => {
                fb.word = !fb.word;
                self.recount();
            }
            (0x52, false, true) => {
                fb.regex = !fb.regex;
                self.recount();
            }
            (0x72, _, _) => self.find_step(!shift),
            (VK_UP | VK_DOWN, false, false) if fb.focus == 0 => self.find_step(vk == VK_DOWN),
            _ => {
                let f = fb.focus;
                if key_cmd(vk, ctrl, shift, alt).is_some()
                    && !matches!(vk, 0x41 | 0x43 | 0x56 | 0x58 | VK_INSERT | VK_DELETE)
                {
                    return false;
                }
                if fb.fields[f].key(vk, ctrl, shift, hwnd) && f == 0 {
                    self.incremental_find();
                }
            }
        }
        self.redraw();
        true
    }

    // ---- mouse -------------------------------------------------------------------------------

    pub fn on_mouse_down(&mut self, x: f32, y: f32, ctrl: bool, shift: bool, alt: bool) {
        self.touch();
        let hit = self.hit_test(x, y);
        if self.palette.is_some() && !matches!(hit, Hit::PalRow(_) | Hit::PalInput | Hit::Overlay) {
            self.palette = None;
        }
        self.pressed = hit;
        match hit {
            Hit::Menu => self.command_palette(),
            Hit::Tab(v, i) => {
                self.views[v].active = i;
                self.av = v;
                self.in_find = false;
                self.drag = Drag::Tab { view: v, idx: i, x0: x, moved: false };
                self.capture();
                self.after_switch();
            }
            Hit::NewTab(v) => {
                self.av = v;
                self.new_file();
            }
            Hit::Text(v) => {
                self.av = v;
                self.in_find = false;
                let pos = self.pos_at(v, x, y);
                let now = Instant::now();
                let (t, lx, ly, n) = self.last_click;
                let clicks = if now.duration_since(t).as_millis() < 450 && (lx - x).abs() < 4.0 && (ly - y).abs() < 4.0 { n % 3 + 1 } else { 1 };
                self.last_click = (now, x, y, clicks);
                if clicks == 2 && self.follow_result() {
                    return;
                }
                let b = self.buf_of_mut(v);
                b.seal = true;
                b.want_col = None;
                if alt {
                    b.sels = vec![Sel::caret(pos)];
                    self.drag = Drag::Select { view: v, anchor: pos, column: true, unit: 0, origin: (pos, pos) };
                } else if clicks == 2 {
                    let (a, z) = b.word_at(pos);
                    let (a, z) = if a == z { (pos, b.doc.next_char(pos)) } else { (a, z) };
                    let n = b.sels.len();
                    b.sels[n - 1] = Sel { anchor: a, head: z };
                    self.drag = Drag::Select { view: v, anchor: a, column: false, unit: 1, origin: (a, z) };
                } else if clicks == 3 {
                    let l = b.doc.line_of(pos);
                    let a = b.doc.line_start(l);
                    let z = b.next_visible(l).map(|n| b.doc.line_start(n)).unwrap_or(b.doc.len());
                    b.sels = vec![Sel { anchor: a, head: z }];
                    self.drag = Drag::Select { view: v, anchor: a, column: false, unit: 2, origin: (a, z) };
                } else if ctrl {
                    b.sels.push(Sel::caret(pos));
                    b.normalize();
                    self.drag = Drag::Select { view: v, anchor: pos, column: false, unit: 0, origin: (pos, pos) };
                } else if shift {
                    let anchor = b.primary().anchor;
                    b.sels = vec![Sel { anchor, head: pos }];
                    self.drag = Drag::Select { view: v, anchor, column: false, unit: 0, origin: (anchor, anchor) };
                } else {
                    b.sels = vec![Sel::caret(pos)];
                    self.drag = Drag::Select { view: v, anchor: pos, column: false, unit: 0, origin: (pos, pos) };
                }
                self.capture();
                self.redraw();
            }
            Hit::Gutter(v) => {
                self.av = v;
                let pos = self.pos_at(v, self.text_rect(v).0, y);
                let (vx, _, _, _) = self.view_rect(v);
                let gw = self.gutter_w(v);
                let b = self.buf_of_mut(v);
                let line = b.doc.line_of(pos);
                if x >= vx + gw - 20.0 {
                    if b.is_folded(line) || b.fold_end(line).is_some() {
                        b.toggle_fold(line);
                    }
                } else if x < vx + 16.0 {
                    b.toggle_bookmark(line);
                } else {
                    let a = b.doc.line_start(line);
                    let z = b.next_visible(line).map(|n| b.doc.line_start(n)).unwrap_or(b.doc.len());
                    b.sels = vec![Sel { anchor: a, head: z }];
                    self.drag = Drag::Select { view: v, anchor: a, column: false, unit: 2, origin: (a, z) };
                    self.capture();
                }
                self.redraw();
            }
            Hit::VScroll(v) => {
                self.av = v;
                if let Some((ty, th)) = self.thumb(v) {
                    if y >= ty && y < ty + th {
                        self.drag = Drag::Scroll { view: v, grab: y - ty };
                    } else {
                        let d = self.vis_rows(v) as isize;
                        self.scroll_by(v, if y < ty { -d } else { d });
                        self.drag = Drag::Scroll { view: v, grab: th / 2.0 };
                    }
                    self.capture();
                }
                self.redraw();
            }
            Hit::Splitter => {
                self.drag = Drag::Splitter;
                self.capture();
            }
            Hit::Status(id) => self.status_click(id),
            Hit::FindField(k) => {
                self.in_find = true;
                if let Some(fb) = self.find.as_mut() {
                    fb.focus = k;
                    let f = &mut fb.fields[k];
                    f.caret = f.text.len();
                    f.anchor = f.caret;
                }
                self.redraw();
            }
            Hit::FindBtn(_) | Hit::Overlay => self.in_find = self.find.is_some(),
            Hit::PalRow(i) => {
                if let Some(p) = self.palette.as_mut() {
                    p.sel = p.scroll + i;
                }
                self.palette_accept();
            }
            _ => {}
        }
    }

    fn capture(&self) {
        unsafe {
            SetCapture(self.hwnd);
        }
    }

    pub fn on_mouse_move(&mut self, x: f32, y: f32) {
        let hit = self.hit_test(x, y);
        if hit != self.hover {
            let chrome = |h: Hit| !matches!(h, Hit::Text(_) | Hit::None);
            let repaint = chrome(hit) || chrome(self.hover);
            self.hover = hit;
            if repaint {
                self.redraw();
            }
        }
        match self.drag {
            Drag::Select { view, anchor, column, unit, origin } => {
                let (_, ty, _, th) = self.text_rect(view);
                if y < ty {
                    self.scroll_by(view, -1);
                } else if y > ty + th {
                    self.scroll_by(view, 1);
                }
                let (tx, _, tw, _) = self.text_rect(view);
                if !self.wrap_on() {
                    if x < tx {
                        self.scroll_x(view, -self.gfx.char_w * 2.0);
                    } else if x > tx + tw {
                        self.scroll_x(view, self.gfx.char_w * 2.0);
                    }
                }
                let pos = self.pos_at(view, x, y.clamp(ty, ty + th - 1.0));
                let b = self.buf_of_mut(view);
                if column {
                    edit::column_select(b, anchor, pos);
                } else {
                    let (a, h) = match unit {
                        1 => {
                            let (wa, wz) = b.word_at(pos);
                            if pos < origin.0 {
                                (origin.1, wa)
                            } else {
                                (origin.0, wz.max(origin.1))
                            }
                        }
                        2 => {
                            let l = b.doc.line_of(pos);
                            if pos < origin.0 {
                                (origin.1, b.doc.line_start(l))
                            } else {
                                (origin.0, b.next_visible(l).map(|n| b.doc.line_start(n)).unwrap_or(b.doc.len()))
                            }
                        }
                        _ => (anchor, pos),
                    };
                    let n = b.sels.len();
                    b.sels[n - 1] = Sel { anchor: a, head: h };
                    b.normalize();
                }
                self.redraw();
            }
            Drag::Scroll { view, grab } => {
                let (_, ty, _, th) = self.text_rect(view);
                if let Some((_, h)) = self.thumb(view) {
                    let f = (y - grab - ty) / (th - h).max(1.0);
                    self.scroll_to_frac(view, f);
                    self.redraw();
                }
            }
            Drag::Splitter => {
                self.split_frac = (x / self.w).clamp(0.15, 0.85);
                self.redraw();
            }
            Drag::Tab { view, idx, x0, moved } => {
                if moved || (x - x0).abs() > 8.0 {
                    let (tabs, _) = self.tab_layout(view);
                    let target = tabs.iter().position(|t| x < t.x + t.w).unwrap_or(tabs.len().saturating_sub(1));
                    let mut idx = idx;
                    if target != idx && target < self.views[view].tabs.len() {
                        let t = self.views[view].tabs.remove(idx);
                        self.views[view].tabs.insert(target, t);
                        self.views[view].active = target;
                        idx = target;
                    }
                    self.drag = Drag::Tab { view, idx, x0, moved: true };
                    self.redraw();
                }
            }
            Drag::None => {}
        }
    }

    pub fn on_mouse_up(&mut self, x: f32, y: f32) {
        unsafe {
            let _ = ReleaseCapture();
        }
        let hit = self.hit_test(x, y);
        let pressed = std::mem::replace(&mut self.pressed, Hit::None);
        if let Drag::Tab { view, moved: true, .. } = self.drag {
            // Dropping a tab onto the other view's tab strip moves it there.
            if let Hit::TabStrip(v2) | Hit::Tab(v2, _) = hit {
                if v2 != view {
                    self.av = view;
                    self.exec(Cmd::MoveOther);
                }
            }
        }
        self.drag = Drag::None;
        if hit == pressed {
            let hwnd = self.hwnd;
            match hit {
                Hit::Min => unsafe {
                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                },
                Hit::Max => unsafe {
                    let _ = ShowWindow(hwnd, if self.maximized { SW_RESTORE } else { SW_MAXIMIZE });
                },
                Hit::Close => unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                },
                Hit::TabClose(v, i) => {
                    self.close_tab(v, i);
                }
                Hit::FindBtn(id) => self.find_button(id),
                _ => {}
            }
        }
        self.redraw();
    }

    pub fn on_middle(&mut self, x: f32, y: f32) {
        if let Hit::Tab(v, i) | Hit::TabClose(v, i) = self.hit_test(x, y) {
            self.close_tab(v, i);
        }
    }

    fn find_button(&mut self, id: usize) {
        let Some(fb) = self.find.as_mut() else { return };
        self.in_find = true;
        match id {
            FB_CASE => fb.case = !fb.case,
            FB_WORD => fb.word = !fb.word,
            FB_REGEX => fb.regex = !fb.regex,
            FB_PREV => self.find_step(false),
            FB_NEXT => self.find_step(true),
            FB_CLOSE => {
                self.find = None;
                self.in_find = false;
            }
            FB_REPL1 => self.replace_one(),
            FB_REPLALL => self.replace_all(),
            FB_MODE => {
                fb.replace = !fb.replace;
                if !fb.replace && fb.focus == 1 {
                    fb.focus = 0;
                }
            }
            FB_SEARCH => self.run_fif(),
            _ => {}
        }
        if matches!(id, FB_CASE | FB_WORD | FB_REGEX) {
            self.recount();
        }
        self.redraw();
    }

    pub fn on_wheel(&mut self, x: f32, y: f32, delta: i32, ctrl: bool, shift: bool, horizontal: bool) {
        if ctrl {
            self.exec(if delta > 0 { Cmd::ZoomIn } else { Cmd::ZoomOut });
            return;
        }
        if let Some((pr, _)) = self.palette_rect() {
            if pr.has(x, y) {
                if let Some(p) = self.palette.as_mut() {
                    let max = p.shown.len().saturating_sub(PAL_MAX);
                    p.scroll = if delta > 0 { p.scroll.saturating_sub(3) } else { (p.scroll + 3).min(max) };
                }
                self.redraw();
                return;
            }
        }
        if y < TITLE_H {
            return;
        }
        let v = self.view_at(x);
        if self.views[v].tabs.is_empty() {
            return;
        }
        if shift || horizontal {
            let d = if horizontal { delta } else { -delta };
            self.scroll_x(v, d as f32 / 120.0 * self.gfx.char_w * 8.0);
        } else {
            let rows = (delta as f32 / 120.0 * 3.0).round() as isize;
            self.scroll_smooth(v, -rows);
        }
        self.redraw();
    }

    pub fn on_context(&mut self, x: f32, y: f32) {
        let hit = self.hit_test(x, y);
        let mut pt = POINT { x: (x * self.scale) as i32, y: (y * self.scale) as i32 };
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut pt);
        }
        match hit {
            Hit::Tab(v, i) => {
                self.views[v].active = i;
                self.av = v;
                self.after_switch();
                let items = [
                    (1, "Close", true),
                    (2, "Close Others", true),
                    (3, "Close All", true),
                    (0, "", true),
                    (4, "Save", true),
                    (5, "Save As…", true),
                    (0, "", true),
                    (6, "Copy Full Path", self.cur_ref().path.is_some()),
                    (7, "Show in Explorer", self.cur_ref().path.is_some()),
                    (0, "", true),
                    (8, "Move to Other View", true),
                    (9, "Clone to Other View", true),
                ];
                let cmd = match sys::popup_menu(self.hwnd, pt, &items) {
                    1 => Cmd::Close,
                    2 => Cmd::CloseOthers,
                    3 => Cmd::CloseAll,
                    4 => Cmd::Save,
                    5 => Cmd::SaveAs,
                    6 => Cmd::CopyPath,
                    7 => Cmd::Reveal,
                    8 => Cmd::MoveOther,
                    9 => Cmd::CloneOther,
                    _ => return,
                };
                self.exec(cmd);
            }
            Hit::Text(v) | Hit::Gutter(v) => {
                self.av = v;
                let pos = self.pos_at(v, x, y);
                let b = self.cur();
                if !b.sels.iter().any(|s| s.lo() <= pos && pos <= s.hi() && !s.is_empty()) {
                    b.set_caret(pos);
                }
                let has_sel = b.sels.iter().any(|s| !s.is_empty());
                let can_undo = b.can_undo();
                self.redraw();
                let items = [
                    (1, "Undo", can_undo),
                    (2, "Redo", true),
                    (0, "", true),
                    (3, "Cut", true),
                    (4, "Copy", true),
                    (5, "Paste", true),
                    (6, "Select All", true),
                    (0, "", true),
                    (7, "Toggle Comment", true),
                    (8, "UPPERCASE", has_sel),
                    (9, "lowercase", has_sel),
                    (0, "", true),
                    (10, "Toggle Bookmark", true),
                    (11, "Toggle Fold", true),
                    (0, "", true),
                    (12, "Command Palette…", true),
                ];
                let cmd = match sys::popup_menu(self.hwnd, pt, &items) {
                    1 => Cmd::Undo,
                    2 => Cmd::Redo,
                    3 => Cmd::Cut,
                    4 => Cmd::Copy,
                    5 => Cmd::Paste,
                    6 => Cmd::SelectAll,
                    7 => Cmd::ToggleComment,
                    8 => Cmd::Upper,
                    9 => Cmd::Lower,
                    10 => Cmd::ToggleBookmark,
                    11 => Cmd::ToggleFold,
                    12 => Cmd::Palette,
                    _ => return,
                };
                self.exec(cmd);
            }
            _ => {}
        }
    }

    fn status_click(&mut self, id: usize) {
        let c = match id {
            0 => Cmd::GotoLine,
            1 => Cmd::TabSize,
            2 => Cmd::Encoding,
            3 => Cmd::LineEnding,
            4 => Cmd::Language,
            5 => Cmd::ZoomReset,
            6 => Cmd::Wrap,
            7 => Cmd::Overtype,
            8 => Cmd::MacroRecord,
            9 => Cmd::InstallUpdate,
            _ => return,
        };
        self.exec(c);
    }

    pub fn drop_files(&mut self, files: Vec<String>) {
        for f in files {
            if std::path::Path::new(&f).is_file() {
                self.open_path(&f);
            }
        }
    }
}
