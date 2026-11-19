//! Geometry of the custom chrome, shared by painting and hit testing.

use crate::app::{App, Hit, CAP_W, SCROLL_W, STATUS_H, TAB_H, TITLE_H};
use crate::settings;

pub const MENU_W: f32 = 46.0;
pub const PAL_ROW: f32 = 30.0;
pub const PAL_INPUT: f32 = 40.0;
pub const PAL_MAX: usize = 12;
pub const FIND_ROW: f32 = 34.0;

/// Find-panel buttons, by index.
pub const FB_CASE: usize = 0;
pub const FB_WORD: usize = 1;
pub const FB_REGEX: usize = 2;
pub const FB_PREV: usize = 3;
pub const FB_NEXT: usize = 4;
pub const FB_CLOSE: usize = 5;
pub const FB_REPL1: usize = 6;
pub const FB_REPLALL: usize = 7;
pub const FB_MODE: usize = 8;
pub const FB_SEARCH: usize = 9;

pub const AVATAR: f32 = 22.0;
/// Avatars shown before "+N".
pub const PEOPLE_MAX: usize = 5;

/// Short file-type label and colour shown on a tab, from the extension.
pub fn badge(b: &crate::buffer::Buffer) -> Option<(String, u32)> {
    if b.diff.is_some() {
        return Some(("DIFF".into(), 0xD29922));
    }
    if b.label.as_deref() == Some("Search results") {
        return Some(("FIND".into(), 0x8B95A7));
    }
    // A co-edited file on another PC: badge from that PC's path.
    let remote = b.collab.as_ref().filter(|c| c.owner.is_some()).map(|c| std::path::PathBuf::from(&c.path));
    let path = b.path.as_ref().or(remote.as_ref())?;
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let color = match ext.as_str() {
        "pcf" | "pcfx" => 0xF0883E,
        "csv" | "tsv" => 0x3FB950,
        "xlsx" | "xls" => 0x3FB950,
        "log" | "out" => 0x8B95A7,
        "txt" | "md" | "markdown" => 0x6E9BD1,
        "rs" => 0xDE7A4A,
        "py" | "pyw" => 0x4B8BBE,
        "js" | "mjs" | "cjs" | "jsx" => 0xE5C04B,
        "ts" | "tsx" => 0x3B82C4,
        "json" | "jsonc" => 0xD9A93B,
        "xml" | "xaml" | "config" | "csproj" | "nwd" | "nwf" => 0xE0703A,
        "html" | "htm" => 0xE34C26,
        "css" | "scss" => 0x5A8FD8,
        "c" | "h" | "cpp" | "hpp" | "cc" => 0x6A8FCF,
        "cs" => 0x9B6BDF,
        "java" | "kt" => 0xC0603A,
        "go" => 0x3CB4C8,
        "ps1" | "psm1" => 0x4A9BE0,
        "bat" | "cmd" | "sh" => 0x7AA06A,
        "sql" => 0xD25A8C,
        "ini" | "cfg" | "toml" | "yml" | "yaml" => 0x9AA3B2,
        _ => 0x8B95A7,
    };
    let mut label: String = ext.chars().take(4).collect::<String>().to_uppercase();
    if label.is_empty() {
        return None;
    }
    if label == "MARKDOWN" {
        label = "MD".into();
    }
    Some((label, color))
}

pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn has(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

pub fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

impl App {
    pub fn caption_buttons(&self) -> [(Hit, Rect); 3] {
        let w = self.w;
        [
            (Hit::Min, r(w - 3.0 * CAP_W, 0.0, CAP_W, TITLE_H)),
            (Hit::Max, r(w - 2.0 * CAP_W, 0.0, CAP_W, TITLE_H)),
            (Hit::Close, r(w - CAP_W, 0.0, CAP_W, TITLE_H)),
        ]
    }

    /// Title-bar avatars of the team: you, then the others online.
    pub fn people_rect(&self) -> Option<Rect> {
        self.team.as_ref()?;
        let n = self.team_online().len().min(PEOPLE_MAX) + 1;
        let extra = if self.team_online().len() > PEOPLE_MAX { 26.0 } else { 0.0 };
        // You stand apart; the others overlap a little.
        let others = (n - 1) as f32;
        let w = AVATAR + if others > 0.0 { 6.0 + AVATAR + (others - 1.0) * (AVATAR - 6.0) } else { 0.0 } + extra + 12.0 + self.shared_pill_w();
        let right = self.w - 3.0 * CAP_W - 8.0 - if self.recording.is_some() { 130.0 } else { 0.0 };
        Some(r(right - w, (TITLE_H - AVATAR) / 2.0 - 2.0, w, AVATAR + 4.0))
    }

    /// Text of the title-bar pill counting files others share live.
    pub fn shared_pill(&self) -> Option<String> {
        match self.shared_by_others() {
            0 => None,
            1 => Some("\u{25CF} 1 file shared live".into()),
            n => Some(format!("\u{25CF} {n} files shared live")),
        }
    }

    pub fn shared_pill_w(&self) -> f32 {
        self.shared_pill().map_or(0.0, |s| self.text_w(&s, true) + 24.0)
    }

    /// The live co-editing card at the top right of view `v`'s text, and
    /// its Stop button, while that view's file is co-edited.
    pub fn live_card(&self, v: usize) -> Option<(Rect, Rect)> {
        let b = self.buf_of(v);
        b.collab.as_ref()?;
        if b.grid.is_some() {
            return None;
        }
        let (line1, line2, btn) = self.live_card_text(v)?;
        let tw = self.text_w(&line1, false).max(self.text_w(&line2, true)) + 34.0;
        let bw = self.text_w(&btn, false) + 26.0;
        let (vx, vy, vw, _) = self.view_rect(v);
        let w = (tw + bw + 22.0).min(vw - SCROLL_W - 24.0).max(200.0);
        let h = 50.0;
        let card = r(vx + vw - SCROLL_W - 12.0 - w, vy + TAB_H + 10.0, w, h);
        let button = r(card.x + card.w - bw - 12.0, card.y + (h - 26.0) / 2.0, bw, 26.0);
        Some((card, button))
    }

    /// (heading, explanation, button) of the live card of view `v`.
    pub fn live_card_text(&self, v: usize) -> Option<(String, String, String)> {
        let bi = self.bi(self.views[v].tabs.get(self.views[v].active)?.buf);
        let c = self.bufs[bi].collab.as_ref()?;
        let people = self.collab_people(bi);
        let who: Vec<&str> = people.iter().map(|s| s.as_str()).collect();
        let head = match c.core.phase {
            crate::collab::Phase::Joining => "Joining\u{2026}".to_string(),
            crate::collab::Phase::Electing | crate::collab::Phase::Resuming => "Reconnecting\u{2026}".to_string(),
            _ if who.is_empty() => "Shared live \u{2013} waiting for others".to_string(),
            _ => format!("Live with {}", crate::team::join_names(&who)),
        };
        let (line2, btn) = match c.owner {
            Some(o) => {
                let name = self.team.as_ref().and_then(|t| t.peers.get(&o)).map(|p| p.name.clone()).unwrap_or_default();
                (format!("{name}\u{2019}s file \u{2013} Ctrl+S saves it on their PC"), "Leave".to_string())
            }
            None if who.is_empty() => ("Only this file is shared \u{2013} others join from Who\u{2019}s Online".to_string(), "Stop sharing".to_string()),
            None => ("Only this file is shared; your other tabs stay private".to_string(), "Stop sharing".to_string()),
        };
        Some((head, line2, btn))
    }

    /// The co-editing pill on a tab: ("LIVE", true) while co-edited here,
    /// ("JOIN", false) while others co-edit it and this Slate could join.
    pub fn tab_pill(&self, bi: usize) -> Option<(&'static str, bool)> {
        if self.bufs[bi].collab.is_some() {
            return Some(("\u{25CF} LIVE", true));
        }
        self.collab_offer_for(bi).map(|_| ("JOIN", false))
    }

    /// Tab rectangles of view `v` and the "+" button.
    pub fn tab_layout(&self, v: usize) -> (Vec<Rect>, Rect) {
        let (vx, y, vw, _) = self.view_rect(v);
        let mut widths: Vec<f32> = self.views[v]
            .tabs
            .iter()
            .map(|t| {
                let b = &self.bufs[self.bi(t.buf)];
                let badge_w = badge(b).map(|(l, _)| self.text_w(&l, true) + 17.0).unwrap_or(0.0);
                let live_w = self.tab_pill(self.bi(t.buf)).map(|(l, _)| self.text_w(l, true) + 17.0).unwrap_or(0.0);
                (self.text_w(&b.name(), false) + 56.0 + badge_w + live_w).clamp(96.0, 320.0)
            })
            .collect();
        let avail = vw - 8.0 - 36.0;
        let total: f32 = widths.iter().sum();
        if total > avail && !widths.is_empty() {
            let each = (avail / widths.len() as f32).max(72.0);
            for w in widths.iter_mut() {
                *w = w.min(each);
            }
        }
        let total: f32 = widths.iter().sum();
        // Keep the active tab in view when tabs still overflow.
        let mut off = 0.0;
        if total > avail {
            let a = self.views[v].active;
            let start: f32 = widths[..a].iter().sum();
            let end = start + widths.get(a).copied().unwrap_or(0.0);
            if end > avail {
                off = end - avail;
            }
        }
        let mut x = vx + 6.0 - off;
        let mut out = Vec::with_capacity(widths.len());
        for w in widths {
            out.push(r(x, y + 5.0, w - 2.0, TAB_H - 5.0));
            x += w;
        }
        let plus = r(x.min(vx + vw - 36.0) + 2.0, y + 6.0, 26.0, TAB_H - 10.0);
        (out, plus)
    }

    pub fn tab_close_rect(t: &Rect) -> Rect {
        r(t.x + t.w - 26.0, t.y + (t.h - 20.0) / 2.0, 20.0, 20.0)
    }

    pub fn palette_rect(&self) -> Option<(Rect, usize)> {
        let p = self.palette.as_ref()?;
        let pw = (self.w - 40.0).min(640.0);
        let rows = p.shown.len().min(PAL_MAX).max(1);
        let h = PAL_INPUT + rows as f32 * PAL_ROW + if rows > 0 { 10.0 } else { 0.0 };
        Some((r((self.w - pw) / 2.0, self.top() + 6.0, pw, h), rows))
    }

    /// Find panel rectangle, the field rectangles and the button rectangles.
    pub fn find_layout(&self) -> Option<(Rect, Vec<(usize, Rect)>, Vec<(usize, Rect)>)> {
        let fb = self.find.as_ref()?;
        let (vx, vy, vw, _) = self.view_rect(self.av);
        let fields = fb.visible_fields();
        let pw = (vw - 40.0).min(if fb.fif { 560.0 } else { 500.0 }).max(260.0);
        let px = vx + vw - SCROLL_W - pw - 10.0;
        let py = vy + TAB_H + 4.0;
        let ph = fields.len() as f32 * FIND_ROW + 10.0;
        let panel = r(px, py, pw, ph);
        let mut fr = Vec::new();
        let mut br = Vec::new();
        let b = 26.0;
        let left = px + 30.0;
        let right_btns = 3.0 * b + 70.0;
        for (k, &f) in fields.iter().enumerate() {
            let y = py + 5.0 + k as f32 * FIND_ROW;
            let fw = pw - 30.0 - right_btns - 8.0;
            fr.push((f, r(left, y + 3.0, fw, FIND_ROW - 6.0)));
            let bx = left + fw + 8.0;
            match f {
                0 => {
                    // Option toggles live inside the find field, at its right edge.
                    for (i, id) in [FB_CASE, FB_WORD, FB_REGEX].iter().enumerate() {
                        br.push((*id, r(left + fw - 3.0 * 24.0 - 2.0 + i as f32 * 24.0, y + 6.0, 22.0, FIND_ROW - 12.0)));
                    }
                    let cx = bx + 66.0;
                    br.push((FB_PREV, r(cx, y + 4.0, b, FIND_ROW - 8.0)));
                    br.push((FB_NEXT, r(cx + b, y + 4.0, b, FIND_ROW - 8.0)));
                    br.push((FB_CLOSE, r(cx + 2.0 * b + 2.0, y + 4.0, b, FIND_ROW - 8.0)));
                }
                1 => {
                    br.push((FB_REPL1, r(bx, y + 4.0, 74.0, FIND_ROW - 8.0)));
                    br.push((FB_REPLALL, r(bx + 78.0, y + 4.0, right_btns - 82.0, FIND_ROW - 8.0)));
                }
                3 => br.push((FB_SEARCH, r(bx, y + 4.0, right_btns - 4.0, FIND_ROW - 8.0))),
                _ => {}
            }
        }
        if !fb.fif {
            br.push((FB_MODE, r(px + 4.0, py + 5.0, 22.0, ph - 10.0)));
        }
        Some((panel, fr, br))
    }

    pub fn hit_test(&self, x: f32, y: f32) -> Hit {
        if let Some((pr, rows)) = self.palette_rect() {
            if pr.has(x, y) {
                if y < pr.y + PAL_INPUT {
                    return Hit::PalInput;
                }
                let i = ((y - pr.y - PAL_INPUT - 5.0) / PAL_ROW).floor().max(0.0) as usize;
                return if i < rows { Hit::PalRow(i) } else { Hit::Overlay };
            }
        }
        if let Some((panel, fields, btns)) = self.find_layout() {
            if panel.has(x, y) {
                for (id, b) in &btns {
                    if b.has(x, y) {
                        return Hit::FindBtn(*id);
                    }
                }
                for (id, f) in &fields {
                    if f.has(x, y) {
                        return Hit::FindField(*id);
                    }
                }
                return Hit::Overlay;
            }
        }
        if y < TITLE_H {
            if x < MENU_W {
                return Hit::Menu;
            }
            for (h, rc) in self.caption_buttons() {
                if rc.has(x, y) {
                    return h;
                }
            }
            if self.people_rect().map_or(false, |rc| rc.has(x, y)) {
                return Hit::People;
            }
            return Hit::Caption;
        }
        if let Some(h) = self.tool_hit(x, y) {
            return h;
        }
        if let Some(part) = self.ai_hit(x, y) {
            return Hit::Ai(part);
        }
        if y >= self.h - STATUS_H {
            for (k, &(x0, x1, id)) in self.status_segs.iter().enumerate() {
                let _ = k;
                if x >= x0 && x < x1 {
                    return Hit::Status(id as usize);
                }
            }
            return Hit::None;
        }
        if self.split {
            let sx = self.view_rect(1).0;
            if (x - sx + 0.5).abs() <= 4.0 {
                return Hit::Splitter;
            }
        }
        let v = self.view_at(x);
        let (vx, vy, vw, _) = self.view_rect(v);
        if y < vy + TAB_H {
            let (tabs, plus) = self.tab_layout(v);
            for (i, t) in tabs.iter().enumerate() {
                if t.has(x, y) {
                    if Self::tab_close_rect(t).has(x, y) {
                        return Hit::TabClose(v, i);
                    }
                    return Hit::Tab(v, i);
                }
            }
            if plus.has(x, y) {
                return Hit::NewTab(v);
            }
            return Hit::TabStrip(v);
        }
        if let Some((card, button)) = self.live_card(v) {
            if card.has(x, y) {
                return Hit::Live(v, button.has(x, y));
            }
        }
        if self.buf_of(v).grid.is_some() && self.buf_of(v).loading.is_none() {
            return Hit::Grid(v);
        }
        if x >= vx + vw - SCROLL_W {
            return Hit::VScroll(v);
        }
        if x < vx + self.gutter_w(v) {
            return Hit::Gutter(v);
        }
        Hit::Text(v)
    }

    pub fn wrap_on(&self) -> bool {
        settings::get().wrap
    }
}
