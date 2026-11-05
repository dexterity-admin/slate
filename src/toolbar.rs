//! The toolbar: the everyday commands as icon buttons under the title bar,
//! for people who would rather click than learn shortcuts or the command
//! palette. Every button runs an ordinary command, so a button and its
//! shortcut always do exactly the same thing.

use crate::app::{App, Cmd, Hit, COMMANDS, TITLE_H};
use crate::gfx::C;
use crate::icons::Icon;
use crate::settings;
use crate::ui::{r, Rect};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

pub const TOOL_H: f32 = 36.0;
const BTN: f32 = 30.0;
const SEP: f32 = 13.0;
const LEFT: f32 = 8.0;
/// Shows the tooltip once the mouse rests on a button.
pub const T_TIP: usize = 12;
const TIP_MS: u32 = 450;

/// The buttons in order; `None` is a separator between groups.
pub static TOOLS: &[Option<(Cmd, Icon, &str)>] = &[
    Some((Cmd::New, Icon::New, "New")),
    Some((Cmd::Open, Icon::Open, "Open\u{2026}")),
    Some((Cmd::Save, Icon::Save, "Save")),
    Some((Cmd::SaveAll, Icon::SaveAll, "Save all")),
    Some((Cmd::Close, Icon::CloseFile, "Close tab")),
    None,
    Some((Cmd::Cut, Icon::Cut, "Cut")),
    Some((Cmd::Copy, Icon::Copy, "Copy")),
    Some((Cmd::Paste, Icon::Paste, "Paste")),
    None,
    Some((Cmd::Undo, Icon::Undo, "Undo")),
    Some((Cmd::Redo, Icon::Redo, "Redo")),
    None,
    Some((Cmd::Find, Icon::Search, "Find")),
    Some((Cmd::Replace, Icon::Replace, "Replace")),
    Some((Cmd::FindInFiles, Icon::FindFiles, "Find in files")),
    None,
    Some((Cmd::ZoomIn, Icon::ZoomIn, "Zoom in")),
    Some((Cmd::ZoomOut, Icon::ZoomOut, "Zoom out")),
    None,
    Some((Cmd::Wrap, Icon::Wrap, "Word wrap")),
    Some((Cmd::Whitespace, Icon::Pilcrow, "Show spaces and line endings")),
    Some((Cmd::SplitView, Icon::Split, "Split view")),
    None,
    Some((Cmd::Compare, Icon::Compare, "Compare with\u{2026}")),
    Some((Cmd::Grid, Icon::Grid, "CSV grid / text")),
    None,
    Some((Cmd::MacroRecord, Icon::Record, "Record macro")),
    Some((Cmd::MacroPlay, Icon::Play, "Play macro")),
    None,
    Some((Cmd::Follow, Icon::Follow, "Follow the file as it grows (tail)")),
    Some((Cmd::CoEdit, Icon::People, "Co-edit with the team")),
    Some((Cmd::Ai, Icon::Spark, "AI assistant")),
];

/// The shortcut of a command, as the command palette shows it.
pub fn shortcut(c: Cmd) -> &'static str {
    COMMANDS.iter().find(|x| x.0 == c).map_or("", |x| x.2)
}

impl App {
    /// Where the views start: under the title bar and the toolbar.
    pub fn top(&self) -> f32 {
        TITLE_H + if self.toolbar { TOOL_H } else { 0.0 }
    }

    /// Button rectangles, by index into TOOLS. Buttons that do not fit the
    /// window are left out, a whole group at a time.
    pub fn tool_layout(&self) -> Vec<(usize, Rect)> {
        let mut out = Vec::new();
        if !self.toolbar {
            return out;
        }
        let right = self.w - LEFT;
        let y = TITLE_H + (TOOL_H - BTN) / 2.0;
        let mut x = LEFT;
        let mut group = Vec::new();
        for (i, t) in TOOLS.iter().enumerate() {
            match t {
                Some(_) => {
                    group.push((i, r(x, y, BTN, BTN)));
                    x += BTN;
                }
                None => {
                    if x > right {
                        return out;
                    }
                    out.append(&mut group);
                    x += SEP;
                }
            }
        }
        if x <= right {
            out.append(&mut group);
        }
        out
    }

    /// Separator positions (x), between the groups shown.
    fn tool_seps(&self, shown: &[(usize, Rect)]) -> Vec<f32> {
        let mut seps = Vec::new();
        for w in shown.windows(2) {
            if TOOLS[w[0].0 + 1].is_none() {
                seps.push(w[0].1.x + BTN + SEP / 2.0);
            }
        }
        seps
    }

    pub fn tool_hit(&self, x: f32, y: f32) -> Option<Hit> {
        if !self.toolbar || y < TITLE_H || y >= TITLE_H + TOOL_H {
            return None;
        }
        Some(self.tool_layout().into_iter().find(|(_, rc)| rc.has(x, y)).map_or(Hit::ToolBar, |(i, _)| Hit::Tool(i)))
    }

    /// (can be used now, switched on) for a button's command.
    fn tool_state(&self, c: Cmd) -> (bool, bool) {
        let b = self.cur_ref();
        let ro = b.read_only();
        let s = settings::get();
        match c {
            Cmd::Save => (b.dirty() && b.diff.is_none(), false),
            Cmd::SaveAll => (self.bufs.iter().any(|b| b.dirty() && b.diff.is_none()), false),
            Cmd::Cut | Cmd::Paste => (!ro, false),
            Cmd::Undo => (!ro && b.can_undo(), false),
            Cmd::Redo => (!ro && b.can_redo(), false),
            Cmd::Replace => (!ro, false),
            Cmd::Wrap => (true, s.wrap),
            Cmd::Whitespace => (true, s.show_ws),
            Cmd::SplitView => (true, self.split),
            Cmd::Compare => (true, self.in_compare()),
            Cmd::Grid => (b.loading.is_none(), b.grid.is_some()),
            Cmd::MacroRecord => (true, self.recording.is_some()),
            Cmd::MacroPlay => (!self.last_macro.is_empty() && self.recording.is_none() && !ro, false),
            Cmd::Follow => (b.path.is_some() && b.diff.is_none(), b.follow.is_some()),
            Cmd::CoEdit => (self.team.is_some() && b.path.is_some() && b.diff.is_none(), b.collab.is_some()),
            Cmd::Ai => (true, self.ai.open),
            _ => (true, false),
        }
    }

    /// The tooltip text of button `i`: what it does and its shortcut.
    fn tool_tip(&self, i: usize) -> (String, &'static str) {
        let Some((c, _, label)) = TOOLS[i] else { return (String::new(), "") };
        let label = match c {
            Cmd::MacroRecord if self.recording.is_some() => "Stop recording",
            Cmd::SplitView if self.split => "Close split view",
            Cmd::Follow if self.cur_ref().follow.is_some() => "Stop following",
            Cmd::CoEdit if self.cur_ref().collab.is_some() => "Leave co-editing",
            Cmd::Grid if self.cur_ref().grid.is_some() => "Back to text",
            _ => label,
        };
        (label.to_string(), shortcut(c))
    }

    pub fn tool_click(&mut self, i: usize) {
        let Some(Some((c, ..))) = TOOLS.get(i) else { return };
        let c = *c;
        self.tip = None;
        if self.tool_state(c).0 {
            self.exec(c);
        }
        self.redraw();
    }

    /// The mouse moved onto `hit`: hide the tooltip, and show the new
    /// button's tooltip after a moment, or at once while one was showing.
    pub fn tool_hover(&mut self, hit: Hit) {
        unsafe {
            let _ = KillTimer(self.hwnd, T_TIP);
        }
        match hit {
            Hit::Tool(i) if self.tip.is_some() => self.tip = Some(i),
            Hit::Tool(_) => unsafe {
                SetTimer(self.hwnd, T_TIP, TIP_MS, None);
            },
            _ => self.tip = None,
        }
    }

    pub fn on_tip_timer(&mut self) {
        unsafe {
            let _ = KillTimer(self.hwnd, T_TIP);
        }
        if let Hit::Tool(i) = self.hover {
            if self.pressed == Hit::None {
                self.tip = Some(i);
                self.redraw();
            }
        }
    }

    pub fn paint_toolbar(&self) {
        if !self.toolbar {
            return;
        }
        let g = &self.gfx;
        g.fill(0.0, TITLE_H, self.w, TOOL_H, C::Chrome);
        let shown = self.tool_layout();
        for x in self.tool_seps(&shown) {
            g.fill(x.round(), TITLE_H + 10.0, 1.0, TOOL_H - 20.0, C::Border);
        }
        let accent = g.b(C::Accent);
        let danger = g.b(C::Danger);
        let faint = g.b(C::Faint);
        for (i, rc) in &shown {
            let Some((c, icon, _)) = TOOLS[*i] else { continue };
            let (enabled, on) = self.tool_state(c);
            let hovered = self.hover == Hit::Tool(*i);
            let pressed = hovered && self.pressed == Hit::Tool(*i);
            if on {
                g.fill_round(rc.x + 1.0, rc.y + 1.0, rc.w - 2.0, rc.h - 2.0, 6.0, C::Sel);
            }
            if enabled && (hovered || pressed) {
                g.fill_alpha(rc.x + 1.0, rc.y + 1.0, rc.w - 2.0, rc.h - 2.0, 6.0, C::Hover, if pressed { 1.0 } else { 0.8 });
                if pressed {
                    g.fill_alpha(rc.x + 1.0, rc.y + 1.0, rc.w - 2.0, rc.h - 2.0, 6.0, C::Border, 0.6);
                }
            }
            // Recording shows a stop square in place of the record dot.
            let icon = if c == Cmd::MacroRecord && on { Icon::Stop } else { icon };
            let (x, y, w, h) = (rc.x, rc.y, rc.w, rc.h);
            g.draw_icon(icon, x, y, w, h, if enabled { C::Fg } else { C::Faint }, 16.0, 1.3);
            let mark = if !enabled {
                faint
            } else if matches!(icon, Icon::CloseFile | Icon::Record | Icon::Stop) {
                danger
            } else {
                accent
            };
            g.draw_icon_accent(icon, x, y, w, h, mark, 16.0, 1.3);
        }
        g.fill(0.0, TITLE_H + TOOL_H - 1.0, self.w, 1.0, C::Border);
    }

    /// The tooltip under the button the mouse rests on.
    pub fn paint_tip(&self) {
        let Some(i) = self.tip else { return };
        let Some((_, rc)) = self.tool_layout().into_iter().find(|(k, _)| *k == i) else { return };
        let g = &self.gfx;
        let f = g.ui_small.as_ref().unwrap();
        let (label, keys) = self.tool_tip(i);
        let lw = g.measure(&label, f);
        let kw = if keys.is_empty() { 0.0 } else { g.measure(keys, f) + 12.0 };
        let (w, h) = (lw + kw + 18.0, 24.0);
        let x = (rc.x + rc.w / 2.0 - w / 2.0).clamp(4.0, (self.w - w - 4.0).max(4.0));
        let y = TITLE_H + TOOL_H + 3.0;
        g.fill_round(x, y, w, h, 5.0, C::Popup);
        unsafe {
            let rr = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT { rect: crate::gfx::rect(x + 0.5, y + 0.5, w - 1.0, h - 1.0), radiusX: 5.0, radiusY: 5.0 };
            g.rt().DrawRoundedRectangle(&rr, g.b(C::Border), 1.0, None);
        }
        g.text(&label, f, x + 9.0, y, lw + 4.0, h, C::Fg);
        if !keys.is_empty() {
            g.text(keys, f, x + 9.0 + lw + 12.0, y, kw, h, C::Dim);
        }
    }
}
