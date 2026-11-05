//! Application state: buffers, views/tabs, commands, files and session.

use crate::anim::Tween;
use crate::buffer::{Buffer, EditKind, Sel};
use crate::edit;
use crate::enc::{self, Enc, Eol};
use crate::gfx::Gfx;
use crate::search::{self, Query};
use crate::settings;
use crate::syntax::{self, LANGS};
use crate::sys;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer, SetWindowTextW, WM_APP};

pub const TITLE_H: f32 = 34.0;
pub const TAB_H: f32 = 32.0;
pub const STATUS_H: f32 = 24.0;
pub const CAP_W: f32 = 46.0;
pub const SCROLL_W: f32 = 12.0;
pub const WM_FIF_DONE: u32 = WM_APP + 1;
pub const T_BENCH: usize = 1;
pub const T_BACKUP: usize = 3;
pub const T_STATUS: usize = 4;
pub const T_LOAD: usize = 8;
pub const T_ANIM: usize = 9;
pub const WM_LOADED: u32 = WM_APP + 5;
/// Files at least this large load on a worker thread.
const ASYNC_LOAD: u64 = 16 << 20;

/// Result of a background load, posted back to the window.
pub struct Loaded {
    pub id: u32,
    pub result: Result<(crate::doc::Doc, Enc, Eol, bool), String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    New, Open, Save, SaveAs, SaveAll, Close, CloseAll, CloseOthers, ReopenClosed, Recent, Reload,
    Reveal, CopyPath, Exit, Undo, Redo, Cut, Copy, Paste, SelectAll, SelectLine, AddNext, ColumnUp,
    ColumnDown, Find, Replace, FindNext, FindPrev, FindInFiles, GotoLine, GotoBrace, Palette, SwitchFile,
    ToggleComment, Comment, Uncomment, Duplicate, DeleteLine, MoveUp, MoveDown, Upper, Lower, TitleCase,
    SortAsc, SortDesc, Unique, Reverse, Trim, Join, TabsToSpaces, SpacesToTabs, ToggleBookmark,
    NextBookmark, PrevBookmark, ClearBookmarks, ToggleFold, FoldAll, UnfoldAll, ZoomIn, ZoomOut,
    ZoomReset, Wrap, Whitespace, Theme, MoveOther, CloneOther, FocusOther, CloseSplit, MacroRecord,
    MacroPlay, Language, Encoding, ReopenEncoding, LineEnding, TabSize, UseTabs, Overtype, NextTab,
    PrevTab, ToggleSession, About, CheckUpdates, InstallUpdate, AutoUpdate, Accent, Animations,
    Grid, GridHeader, GridDelim, GridClear, GridFit, GridCopyHeaders, GridDefault,
    Compare, CompareFile, CompareSaved, CompareNext, ComparePrev, CompareRefresh, CompareSwap,
    CompareWs, CompareCase, CompareClose,
}

pub static COMMANDS: &[(Cmd, &str, &str)] = &[
    (Cmd::New, "File: New", "Ctrl+N"),
    (Cmd::Open, "File: Open…", "Ctrl+O"),
    (Cmd::Save, "File: Save", "Ctrl+S"),
    (Cmd::SaveAs, "File: Save As…", "Ctrl+Alt+S"),
    (Cmd::SaveAll, "File: Save All", "Ctrl+Shift+S"),
    (Cmd::Close, "File: Close Tab", "Ctrl+W"),
    (Cmd::CloseOthers, "File: Close Other Tabs", ""),
    (Cmd::CloseAll, "File: Close All Tabs", ""),
    (Cmd::ReopenClosed, "File: Reopen Closed Tab", "Ctrl+Shift+T"),
    (Cmd::Recent, "File: Open Recent…", "Ctrl+Shift+O"),
    (Cmd::Reload, "File: Reload from Disk", ""),
    (Cmd::Reveal, "File: Show in Explorer", ""),
    (Cmd::CopyPath, "File: Copy Full Path", ""),
    (Cmd::SwitchFile, "File: Go to Open Tab…", "Ctrl+P"),
    (Cmd::Exit, "File: Exit", "Alt+F4"),
    (Cmd::Undo, "Edit: Undo", "Ctrl+Z"),
    (Cmd::Redo, "Edit: Redo", "Ctrl+Y"),
    (Cmd::Cut, "Edit: Cut", "Ctrl+X"),
    (Cmd::Copy, "Edit: Copy", "Ctrl+C"),
    (Cmd::Paste, "Edit: Paste", "Ctrl+V"),
    (Cmd::SelectAll, "Edit: Select All", "Ctrl+A"),
    (Cmd::SelectLine, "Selection: Select Line", "Ctrl+Alt+L"),
    (Cmd::AddNext, "Selection: Add Next Occurrence", "Ctrl+E"),
    (Cmd::ColumnUp, "Selection: Add Caret Above", "Ctrl+Alt+Up"),
    (Cmd::ColumnDown, "Selection: Add Caret Below", "Ctrl+Alt+Down"),
    (Cmd::ToggleComment, "Edit: Toggle Line Comment", "Ctrl+Q"),
    (Cmd::Comment, "Edit: Comment Lines", "Ctrl+K"),
    (Cmd::Uncomment, "Edit: Uncomment Lines", "Ctrl+Shift+K"),
    (Cmd::Duplicate, "Edit: Duplicate Line / Selection", "Ctrl+D"),
    (Cmd::DeleteLine, "Edit: Delete Line", "Ctrl+Shift+L"),
    (Cmd::MoveUp, "Edit: Move Line Up", "Ctrl+Shift+Up"),
    (Cmd::MoveDown, "Edit: Move Line Down", "Ctrl+Shift+Down"),
    (Cmd::Join, "Edit: Join Lines", "Ctrl+J"),
    (Cmd::Upper, "Edit: UPPERCASE", "Ctrl+Shift+U"),
    (Cmd::Lower, "Edit: lowercase", "Ctrl+U"),
    (Cmd::TitleCase, "Edit: Title Case", ""),
    (Cmd::SortAsc, "Lines: Sort Ascending", ""),
    (Cmd::SortDesc, "Lines: Sort Descending", ""),
    (Cmd::Unique, "Lines: Remove Duplicate Lines", ""),
    (Cmd::Reverse, "Lines: Reverse Order", ""),
    (Cmd::Trim, "Lines: Trim Trailing Whitespace", ""),
    (Cmd::TabsToSpaces, "Whitespace: Convert Leading Tabs to Spaces", ""),
    (Cmd::SpacesToTabs, "Whitespace: Convert Leading Spaces to Tabs", ""),
    (Cmd::Overtype, "Edit: Toggle Insert / Overtype", "Insert"),
    (Cmd::Find, "Search: Find…", "Ctrl+F"),
    (Cmd::Replace, "Search: Replace…", "Ctrl+H"),
    (Cmd::FindNext, "Search: Find Next", "F3"),
    (Cmd::FindPrev, "Search: Find Previous", "Shift+F3"),
    (Cmd::FindInFiles, "Search: Find in Files…", "Ctrl+Shift+F"),
    (Cmd::GotoLine, "Search: Go to Line…", "Ctrl+G"),
    (Cmd::GotoBrace, "Search: Go to Matching Brace", "Ctrl+B"),
    (Cmd::ToggleBookmark, "Bookmark: Toggle", "Ctrl+F2"),
    (Cmd::NextBookmark, "Bookmark: Next", "F2"),
    (Cmd::PrevBookmark, "Bookmark: Previous", "Shift+F2"),
    (Cmd::ClearBookmarks, "Bookmark: Clear All", ""),
    (Cmd::ToggleFold, "Fold: Toggle Current Block", "Ctrl+Alt+F"),
    (Cmd::FoldAll, "Fold: Fold All", "Alt+0"),
    (Cmd::UnfoldAll, "Fold: Unfold All", "Alt+Shift+0"),
    (Cmd::ZoomIn, "View: Zoom In", "Ctrl+="),
    (Cmd::ZoomOut, "View: Zoom Out", "Ctrl+-"),
    (Cmd::ZoomReset, "View: Reset Zoom", "Ctrl+0"),
    (Cmd::Wrap, "View: Toggle Word Wrap", "Alt+Z"),
    (Cmd::Whitespace, "View: Toggle Whitespace and Line Endings", ""),
    (Cmd::Theme, "View: Toggle Light / Dark Theme", ""),
    (Cmd::Accent, "View: Accent Colour…", ""),
    (Cmd::Animations, "View: Toggle Animations", ""),
    (Cmd::MoveOther, "View: Move Tab to Other View", ""),
    (Cmd::CloneOther, "View: Clone Tab to Other View", ""),
    (Cmd::FocusOther, "View: Focus Other View", "F8"),
    (Cmd::CloseSplit, "View: Close Split", ""),
    (Cmd::NextTab, "View: Next Tab", "Ctrl+Tab"),
    (Cmd::PrevTab, "View: Previous Tab", "Ctrl+Shift+Tab"),
    (Cmd::Grid, "View: Toggle CSV Grid / Text", "Ctrl+Shift+G"),
    (Cmd::GridHeader, "Grid: Toggle First Row as Header", ""),
    (Cmd::GridDelim, "Grid: Set Column Delimiter…", ""),
    (Cmd::GridClear, "Grid: Clear Filters and Sort", ""),
    (Cmd::GridFit, "Grid: Autofit Column Widths", ""),
    (Cmd::GridCopyHeaders, "Grid: Copy with Headers", ""),
    (Cmd::GridDefault, "Settings: Toggle Opening CSV Files as a Grid", ""),
    (Cmd::Compare, "Compare: Compare With\u{2026} / Close Compare", "Ctrl+Alt+C"),
    (Cmd::CompareFile, "Compare: Compare With File\u{2026}", ""),
    (Cmd::CompareSaved, "Compare: Compare With Saved Version", ""),
    (Cmd::CompareNext, "Compare: Next Difference", "F7"),
    (Cmd::ComparePrev, "Compare: Previous Difference", "Shift+F7"),
    (Cmd::CompareRefresh, "Compare: Refresh", "F5"),
    (Cmd::CompareSwap, "Compare: Swap Sides", ""),
    (Cmd::CompareWs, "Compare: Toggle Ignore Whitespace", ""),
    (Cmd::CompareCase, "Compare: Toggle Ignore Case", ""),
    (Cmd::CompareClose, "Compare: Close Compare", ""),
    (Cmd::Language, "Document: Set Language…", ""),
    (Cmd::Encoding, "Document: Convert Encoding…", ""),
    (Cmd::ReopenEncoding, "Document: Reopen with Encoding…", ""),
    (Cmd::LineEnding, "Document: Set Line Endings…", ""),
    (Cmd::TabSize, "Document: Set Tab Width…", ""),
    (Cmd::UseTabs, "Document: Toggle Indent with Tabs / Spaces", ""),
    (Cmd::MacroRecord, "Macro: Start / Stop Recording", "Ctrl+Shift+R"),
    (Cmd::MacroPlay, "Macro: Play Back", "Ctrl+Shift+M"),
    (Cmd::ToggleSession, "Settings: Toggle Restore Session on Start", ""),
    (Cmd::CheckUpdates, "Help: Check for Updates", ""),
    (Cmd::InstallUpdate, "Help: Install Available Update", ""),
    (Cmd::AutoUpdate, "Settings: Toggle Automatic Update Checks", ""),
    (Cmd::About, "Help: About Slate", ""),
];

// ---- small widgets ------------------------------------------------------------

#[derive(Default, Clone)]
pub struct Input {
    pub text: String,
    pub caret: usize,
    pub anchor: usize,
}

impl Input {
    pub fn set(&mut self, s: &str) {
        self.text = s.to_string();
        self.anchor = 0;
        self.caret = self.text.len();
    }
    fn prev(&self, i: usize) -> usize {
        self.text[..i].char_indices().last().map(|(k, _)| k).unwrap_or(0)
    }
    fn next(&self, i: usize) -> usize {
        self.text[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(i)
    }
    pub fn sel(&self) -> (usize, usize) {
        (self.anchor.min(self.caret), self.anchor.max(self.caret))
    }
    pub fn insert(&mut self, s: &str) {
        let (a, b) = self.sel();
        let s: String = s.chars().filter(|c| *c != '\r' && *c != '\n').collect();
        self.text.replace_range(a..b, &s);
        self.caret = a + s.len();
        self.anchor = self.caret;
    }
    /// Handles an editing key; returns true when the text changed.
    pub fn key(&mut self, vk: u32, ctrl: bool, shift: bool, hwnd: HWND) -> bool {
        let (a, b) = self.sel();
        match vk {
            0x08 => {
                if a == b && a > 0 {
                    self.anchor = if ctrl { 0 } else { self.prev(a) };
                }
                self.insert("");
                return true;
            }
            0x2E => {
                if a == b && b < self.text.len() {
                    self.anchor = self.next(b);
                }
                self.insert("");
                return true;
            }
            0x25 => self.caret = if !shift && a != b { a } else { self.prev(self.caret) },
            0x27 => self.caret = if !shift && a != b { b } else { self.next(self.caret) },
            0x24 => self.caret = 0,
            0x23 => self.caret = self.text.len(),
            0x41 if ctrl => {
                self.anchor = 0;
                self.caret = self.text.len();
                return false;
            }
            0x43 | 0x58 if ctrl => {
                if a != b {
                    sys::set_clipboard(hwnd, self.text[a..b].as_bytes(), false);
                    if vk == 0x58 {
                        self.insert("");
                        return true;
                    }
                }
                return false;
            }
            0x56 if ctrl => {
                if let Some((t, _)) = sys::get_clipboard(hwnd) {
                    self.insert(&String::from_utf8_lossy(&t));
                    return true;
                }
                return false;
            }
            _ => return false,
        }
        if !shift {
            self.anchor = self.caret;
        }
        false
    }
}

#[derive(Clone)]
pub enum PalAct {
    Cmd(Cmd),
    Tab(usize, usize),
    Open(String),
    Lang(usize),
    Enc(Enc),
    Reopen(Enc),
    Eol(Eol),
    TabSize(usize),
    Accent(usize),
    Delim(u8),
    Compare(u32),
}

pub struct PalItem {
    pub label: String,
    pub hint: String,
    pub act: PalAct,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PalMode {
    Commands,
    Goto,
    List,
}

pub struct Palette {
    pub mode: PalMode,
    pub title: String,
    pub input: Input,
    pub items: Vec<PalItem>,
    pub shown: Vec<usize>,
    pub sel: usize,
    pub scroll: usize,
}

pub struct FindBar {
    pub replace: bool,
    pub fif: bool,
    pub fields: [Input; 4],
    pub focus: usize,
    pub case: bool,
    pub word: bool,
    pub regex: bool,
    pub count: Option<usize>,
    pub err: Option<String>,
}

impl FindBar {
    pub fn query(&self) -> Query {
        Query { pat: self.fields[0].text.clone(), case: self.case, word: self.word, regex: self.regex }
    }
    /// Fields shown for the current mode: find, replace, folder, filter.
    pub fn visible_fields(&self) -> Vec<usize> {
        if self.fif {
            vec![0, 2, 3]
        } else if self.replace {
            vec![0, 1]
        } else {
            vec![0]
        }
    }
}

pub struct Tab {
    pub buf: u32,
    pub top: usize,
    pub sub: usize,
    pub x: f32,
}

#[derive(Default)]
pub struct View {
    pub tabs: Vec<Tab>,
    pub active: usize,
}

pub struct LineLayout {
    pub layout: IDWriteTextLayout,
    /// UTF-16 index -> byte offset within the line; one extra entry at the end.
    pub map: Vec<u32>,
    pub rows: usize,
    pub width: f32,
    pub used: u32,
}

impl LineLayout {
    pub fn to16(&self, byte: usize) -> u32 {
        self.map.partition_point(|&b| (b as usize) < byte).min(self.map.len() - 1) as u32
    }
    pub fn to_byte(&self, i16: u32) -> usize {
        self.map[(i16 as usize).min(self.map.len() - 1)] as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    None,
    Menu,
    Min,
    Max,
    Close,
    Caption,
    Tab(usize, usize),
    TabClose(usize, usize),
    NewTab(usize),
    TabStrip(usize),
    Gutter(usize),
    Text(usize),
    VScroll(usize),
    Splitter,
    Status(usize),
    FindField(usize),
    FindBtn(usize),
    PalRow(usize),
    PalInput,
    Overlay,
    Grid(usize),
}

pub enum Drag {
    None,
    Select { view: usize, anchor: usize, column: bool, unit: u8, origin: (usize, usize) },
    Scroll { view: usize, grab: f32 },
    Splitter,
    Tab { view: usize, idx: usize, x0: f32, moved: bool },
    /// Cell selection; mode 0 cells, 1 whole rows, 2 whole columns.
    Grid { view: usize, mode: u8 },
    GridCol { view: usize, col: usize, x0: f32, w0: f32 },
    GridV { view: usize, grab: f32 },
    GridH { view: usize, grab: f32 },
}

#[derive(Clone)]
pub enum Action {
    Char(char),
    Key(u32, bool, bool, bool),
}

pub struct App {
    pub hwnd: HWND,
    pub gfx: Gfx,
    pub bufs: Vec<Buffer>,
    next_id: u32,
    next_untitled: usize,
    pub views: [View; 2],
    pub split: bool,
    pub split_frac: f32,
    pub av: usize,
    pub w: f32,
    pub h: f32,
    pub scale: f32,
    pub maximized: bool,
    pub focused: bool,
    pub hover: Hit,
    pub pressed: Hit,
    pub drag: Drag,
    pub palette: Option<Palette>,
    pub find: Option<FindBar>,
    /// Keyboard goes to the find panel rather than the editor.
    pub in_find: bool,
    pub last_find: Query,
    pub repl_text: String,
    pub last_input: Instant,
    /// Where the focused caret goes this frame: (x, y, w, h) in DIPs.
    pub caret_target: Cell<Option<(f32, f32, f32, f32)>>,
    /// Size of the created system caret, in pixels.
    pub sys_caret: Option<(i32, i32)>,
    pub caret_shown: bool,
    /// Measured UI text widths, keyed by (text, small font).
    pub widths: RefCell<HashMap<(String, bool), f32>>,
    pub backup_pending: bool,
    pub status: Option<String>,
    pub closed: Vec<String>,
    pub recording: Option<Vec<Action>>,
    pub last_macro: Vec<Action>,
    pub replaying: bool,
    pub overtype: bool,
    pub results_buf: Option<u32>,
    pub cache: HashMap<u64, LineLayout>,
    pub cache_tick: u32,
    pub font_gen: u64,
    pub last_click: (Instant, f32, f32, u8),
    pub status_segs: Vec<(f32, f32, u8)>,
    /// `-nosession`: never read or write the session or backups.
    pub no_session: bool,
    pub upd: crate::updater_ui::UpdateState,
    /// Smooth scroll: pixel offset per view easing back to 0.
    pub scroll_anim: [Option<Tween>; 2],
    /// Active-tab underline per view: (x tween, width tween, target x, target w).
    pub tab_ind: RefCell<[Option<(Tween, Tween, f32, f32)>; 2]>,
    pub pal_since: Option<Instant>,
    pub find_since: Option<Instant>,
    pub anim_timer: bool,
    /// The new version took over; close without saving the session again.
    pub exiting_for_update: bool,
    /// Grid part under the mouse that shows a hover state.
    pub grid_hover: crate::grid::GHit,
    /// Last scroll state both compare sides were synced to.
    pub cmp_sync: Option<(usize, usize, f32)>,
}

pub fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

impl App {
    pub fn new(hwnd: HWND, gfx: Gfx) -> Self {
        App {
            hwnd,
            gfx,
            bufs: Vec::new(),
            next_id: 1,
            next_untitled: 1,
            views: [View::default(), View::default()],
            split: false,
            split_frac: 0.5,
            av: 0,
            w: 1000.0,
            h: 700.0,
            scale: 1.0,
            maximized: false,
            focused: true,
            hover: Hit::None,
            pressed: Hit::None,
            drag: Drag::None,
            palette: None,
            find: None,
            in_find: false,
            last_find: Query::default(),
            repl_text: String::new(),
            last_input: Instant::now(),
            caret_target: Cell::new(None),
            sys_caret: None,
            caret_shown: false,
            widths: RefCell::new(HashMap::new()),
            backup_pending: false,
            status: None,
            closed: Vec::new(),
            recording: None,
            last_macro: Vec::new(),
            replaying: false,
            overtype: false,
            results_buf: None,
            cache: HashMap::new(),
            cache_tick: 0,
            font_gen: 1,
            last_click: (Instant::now(), 0.0, 0.0, 0),
            status_segs: Vec::new(),
            no_session: false,
            upd: Default::default(),
            scroll_anim: [None, None],
            tab_ind: RefCell::new([None, None]),
            pal_since: None,
            find_since: None,
            anim_timer: false,
            exiting_for_update: false,
            grid_hover: crate::grid::GHit::None,
            cmp_sync: None,
        }
    }

    pub fn redraw(&self) {
        invalidate(self.hwnd);
    }

    // ---- buffer / view access ---------------------------------------------------

    pub fn bi(&self, id: u32) -> usize {
        self.bufs.iter().position(|b| b.id == id).unwrap_or(0)
    }

    pub fn tab(&self, v: usize) -> &Tab {
        let vw = &self.views[v];
        &vw.tabs[vw.active]
    }

    pub fn tab_mut(&mut self, v: usize) -> &mut Tab {
        let vw = &mut self.views[v];
        &mut vw.tabs[vw.active]
    }

    pub fn buf_of(&self, v: usize) -> &Buffer {
        &self.bufs[self.bi(self.tab(v).buf)]
    }

    pub fn buf_of_mut(&mut self, v: usize) -> &mut Buffer {
        let i = self.bi(self.tab(v).buf);
        &mut self.bufs[i]
    }

    pub fn cur(&mut self) -> &mut Buffer {
        self.buf_of_mut(self.av)
    }

    pub fn cur_ref(&self) -> &Buffer {
        self.buf_of(self.av)
    }

    pub fn new_buffer(&mut self, text: Vec<u8>) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let mut b = Buffer::new(id, text);
        b.tab_size = settings::get().tab_size;
        self.bufs.push(b);
        id
    }

    pub(crate) fn add_tab(&mut self, v: usize, id: u32) {
        let vw = &mut self.views[v];
        let at = if vw.tabs.is_empty() { 0 } else { vw.active + 1 };
        vw.tabs.insert(at, Tab { buf: id, top: 0, sub: 0, x: 0.0 });
        vw.active = at;
        self.av = v;
    }

    pub fn new_file(&mut self) {
        let id = self.new_buffer(Vec::new());
        let n = self.next_untitled;
        self.next_untitled += 1;
        self.bufs.last_mut().unwrap().untitled = n;
        self.add_tab(self.av, id);
        self.after_switch();
    }

    /// Pristine "new N" tab that a newly opened file may replace.
    fn pristine_active(&self) -> Option<u32> {
        let vw = &self.views[self.av];
        let t = vw.tabs.get(vw.active)?;
        let b = &self.bufs[self.bi(t.buf)];
        (b.path.is_none() && b.doc.len() == 0 && !b.can_undo() && self.views[1 - self.av].tabs.iter().all(|x| x.buf != t.buf)).then_some(t.buf)
    }

    pub fn find_open(&self, path: &Path) -> Option<(usize, usize)> {
        for v in 0..2 {
            for (i, t) in self.views[v].tabs.iter().enumerate() {
                if let Some(p) = &self.bufs[self.bi(t.buf)].path {
                    if p.as_os_str().eq_ignore_ascii_case(path.as_os_str()) {
                        return Some((v, i));
                    }
                }
            }
        }
        None
    }

    pub fn open_path(&mut self, path: &str) -> bool {
        let pb = std::fs::canonicalize(path).map(strip_unc).unwrap_or_else(|_| PathBuf::from(path));
        if let Some((v, i)) = self.find_open(&pb) {
            self.views[v].active = i;
            self.av = v;
            self.after_switch();
            return true;
        }
        let size = std::fs::metadata(&pb).map(|m| m.len()).unwrap_or(0);
        if size >= ASYNC_LOAD {
            return self.open_async(pb, size);
        }
        let raw = match crate::doc::read_for_doc(&pb) {
            Ok(r) => r,
            Err(e) => {
                self.flash(&format!("Cannot open {}: {e}", pb.display()));
                return false;
            }
        };
        let replace = self.pristine_active();
        let d = enc::decode(raw, None);
        let id = self.new_buffer(d.text);
        {
            let b = self.bufs.last_mut().unwrap();
            b.enc = d.enc;
            b.eol = d.eol;
            b.mixed_eol = d.mixed_eol;
            b.lang = syntax::lang_for_path(&pb.to_string_lossy());
            b.mtime = std::fs::metadata(&pb).and_then(|m| m.modified()).ok();
            b.path = Some(pb.clone());
        }
        self.add_tab(self.av, id);
        if let Some(old) = replace {
            self.views[self.av].tabs.retain(|t| t.buf != old);
            self.views[self.av].active = self.views[self.av].tabs.iter().position(|t| t.buf == id).unwrap_or(0);
            self.bufs.retain(|b| b.id != old);
        }
        settings::push_recent(&pb.to_string_lossy());
        if d.mixed_eol {
            self.flash(&format!("{}: mixed line endings unified to {} (applies on save)", pb.display(), d.eol.name()));
        }
        self.auto_grid(id);
        self.after_switch();
        true
    }

    /// Opens a large file on a worker thread; its tab shows progress until
    /// the text arrives in `on_loaded`.
    fn open_async(&mut self, pb: PathBuf, size: u64) -> bool {
        use std::sync::atomic::AtomicU64;
        use std::sync::Arc;
        let replace = self.pristine_active();
        let id = self.new_buffer(Vec::new());
        let progress = Arc::new(AtomicU64::new(0));
        {
            let b = self.bufs.last_mut().unwrap();
            b.path = Some(pb.clone());
            b.lang = syntax::lang_for_path(&pb.to_string_lossy());
            b.loading = Some((progress.clone(), size));
        }
        self.add_tab(self.av, id);
        if let Some(old) = replace {
            self.views[self.av].tabs.retain(|t| t.buf != old);
            self.views[self.av].active = self.views[self.av].tabs.iter().position(|t| t.buf == id).unwrap_or(0);
            self.bufs.retain(|b| b.id != old);
        }
        settings::push_recent(&pb.to_string_lossy());
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let result = crate::doc::read_for_doc_progress(&pb, Some(&progress)).map_err(|e| e.to_string()).map(|raw| {
                let d = enc::decode(raw, None);
                (crate::doc::Doc::load(d.text), d.enc, d.eol, d.mixed_eol)
            });
            let p = Box::into_raw(Box::new(Loaded { id, result }));
            unsafe {
                if PostMessageW(HWND(hwnd as *mut _), WM_LOADED, WPARAM(0), LPARAM(p as isize)).is_err() {
                    drop(Box::from_raw(p));
                }
            }
        });
        unsafe {
            SetTimer(self.hwnd, T_LOAD, 120, None);
        }
        self.after_switch();
        true
    }

    pub fn on_loaded(&mut self, l: Loaded) {
        // The tab may have been closed while loading.
        if let Some(i) = self.bufs.iter().position(|b| b.id == l.id && b.loading.is_some()) {
            match l.result {
                Ok((doc, enc, eol, mixed)) => {
                    let b = &mut self.bufs[i];
                    b.doc = doc;
                    b.enc = enc;
                    b.eol = eol;
                    b.mixed_eol = mixed;
                    b.loading = None;
                    b.states.clear();
                    b.gen += 1;
                    b.sels = vec![Sel::default()];
                    b.mtime = b.path.as_ref().and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
                    let name = b.name();
                    if mixed {
                        self.flash(&format!("{name}: mixed line endings unified to {} (applies on save)", eol.name()));
                    }
                    self.auto_grid(l.id);
                }
                Err(e) => {
                    let name = self.bufs[i].name();
                    self.bufs[i].loading = None;
                    if let Some((v, t)) = self.find_open_id(l.id) {
                        self.close_tab(v, t);
                    }
                    self.flash(&format!("Cannot open {name}: {e}"));
                }
            }
        }
        if !self.bufs.iter().any(|b| b.loading.is_some()) {
            unsafe {
                let _ = KillTimer(self.hwnd, T_LOAD);
            }
        }
        self.after_switch();
    }

    pub fn open_dialog(&mut self) {
        let dir = self.cur_ref().path.as_ref().and_then(|p| p.parent()).map(|p| p.to_string_lossy().into_owned());
        for f in sys::open_dialog(self.hwnd, dir.as_deref()) {
            self.open_path(&f);
        }
    }

    pub fn save_buf(&mut self, id: u32, save_as: bool) -> bool {
        let i = self.bi(id);
        let path = match (&self.bufs[i].path, save_as) {
            (Some(p), false) => p.clone(),
            (p, _) => {
                let name = self.bufs[i].name();
                let suggested = if name.starts_with("new ") { format!("{name}.txt") } else { name };
                let dir = p.as_ref().and_then(|p| p.parent()).map(|p| p.to_string_lossy().into_owned());
                match sys::save_dialog(self.hwnd, &suggested, dir.as_deref()) {
                    Some(s) => PathBuf::from(s),
                    None => return false,
                }
            }
        };
        let b = &mut self.bufs[i];
        let bytes = enc::encode(&b.text_all(), b.enc, b.eol);
        if let Err(e) = std::fs::write(&path, bytes) {
            let msg = format!("Could not save {}:\n{e}", path.display());
            sys::info(self.hwnd, &msg);
            return false;
        }
        let new_name = b.path.as_ref() != Some(&path);
        b.path = Some(path.clone());
        b.mark_saved();
        b.mixed_eol = false;
        b.mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if new_name {
            let lang = syntax::lang_for_path(&path.to_string_lossy());
            if lang != 0 || b.lang == 0 {
                b.set_lang(lang);
            }
        }
        if let Some(bk) = b.backup.take() {
            let _ = std::fs::remove_file(settings::backup_dir().join(bk));
        }
        settings::push_recent(&path.to_string_lossy());
        self.flash(&format!("Saved {}", path.display()));
        self.update_title();
        true
    }

    /// Closes tab `idx` of view `v`; false when the user cancelled.
    pub fn close_tab(&mut self, v: usize, idx: usize) -> bool {
        let id = self.views[v].tabs[idx].buf;
        let elsewhere = self.views[1 - v].tabs.iter().any(|t| t.buf == id);
        let i = self.bi(id);
        if !elsewhere && self.bufs[i].dirty() && Some(id) != self.results_buf {
            let name = self.bufs[i].name();
            self.views[v].active = idx;
            self.av = v;
            self.redraw();
            match sys::ask_save(self.hwnd, &name) {
                sys::Answer::Yes => {
                    if !self.save_buf(id, false) {
                        return false;
                    }
                }
                sys::Answer::No => {}
                sys::Answer::Cancel => return false,
            }
        }
        self.views[v].tabs.remove(idx);
        let partner = self.bufs[i].diff.as_ref().map(|d| d.partner).filter(|_| !elsewhere);
        if !elsewhere {
            let i = self.bi(id);
            if let Some(p) = &self.bufs[i].path {
                self.closed.push(p.to_string_lossy().into_owned());
            }
            if let Some(bk) = &self.bufs[i].backup {
                let _ = std::fs::remove_file(settings::backup_dir().join(bk));
            }
            self.bufs.remove(i);
            if self.results_buf == Some(id) {
                self.results_buf = None;
            }
        }
        let vw = &mut self.views[v];
        if vw.active >= vw.tabs.len() || (vw.active > idx) {
            vw.active = vw.active.saturating_sub(1);
        }
        if self.views[v].tabs.is_empty() {
            if self.split {
                self.split = false;
                if v == 0 {
                    self.views.swap(0, 1);
                }
                self.av = 0;
            } else {
                self.new_file();
            }
        }
        // The two sides of a compare close together.
        if let Some((pv, pi)) = partner.and_then(|p| self.find_open_id(p)) {
            return self.close_tab(pv, pi);
        }
        self.after_switch();
        true
    }

    pub fn close_where(&mut self, keep: Option<usize>) {
        let v = self.av;
        let mut i = self.views[v].tabs.len();
        let keep_id = keep.map(|k| self.views[v].tabs[k].buf);
        while i > 0 {
            i -= 1;
            if i < self.views[v].tabs.len() && Some(self.views[v].tabs[i].buf) != keep_id {
                if !self.close_tab(v, i) {
                    return;
                }
                if self.av != v {
                    return;
                }
            }
        }
    }

    pub fn after_switch(&mut self) {
        self.update_title();
        self.recount();
        self.redraw();
    }

    pub fn update_title(&self) {
        let b = self.cur_ref();
        let t = match &b.path {
            Some(p) => format!("{}{} - Slate", if b.dirty() { "● " } else { "" }, p.display()),
            None => format!("{}{} - Slate", if b.dirty() { "● " } else { "" }, b.name()),
        };
        let w = sys::wide(&t);
        unsafe {
            let _ = SetWindowTextW(self.hwnd, windows::core::PCWSTR(w.as_ptr()));
        }
    }

    pub fn flash(&mut self, msg: &str) {
        self.status = Some(msg.to_string());
        unsafe {
            SetTimer(self.hwnd, T_STATUS, 5000, None);
        }
        self.redraw();
    }

    // ---- input bookkeeping and backups -------------------------------------------------

    pub fn touch(&mut self) {
        self.last_input = Instant::now();
    }

    /// Width of UI text, measured once per string.
    pub fn text_w(&self, s: &str, small: bool) -> f32 {
        let key = (s.to_string(), small);
        if let Some(w) = self.widths.borrow().get(&key) {
            return *w;
        }
        let f = if small { self.gfx.ui_small.as_ref() } else { self.gfx.ui.as_ref() };
        let w = f.map(|f| self.gfx.measure(s, f)).unwrap_or(80.0);
        let mut m = self.widths.borrow_mut();
        if m.len() > 512 {
            m.clear();
        }
        m.insert(key, w);
        w
    }

    pub fn edited(&mut self) {
        if !self.backup_pending {
            self.backup_pending = true;
            unsafe {
                SetTimer(self.hwnd, T_BACKUP, 8000, None);
            }
        }
        self.update_title();
    }

    /// Writes changed dirty buffers to the backup folder (crash safety).
    pub fn write_backups(&mut self) {
        self.backup_pending = false;
        unsafe {
            let _ = KillTimer(self.hwnd, T_BACKUP);
        }
        if !settings::get().restore_session || self.no_session {
            return;
        }
        let dir = settings::backup_dir();
        let mut made_dir = false;
        for b in self.bufs.iter_mut() {
            if Some(b.id) == self.results_buf {
                continue;
            }
            if !b.dirty() {
                if let Some(bk) = b.backup.take() {
                    let _ = std::fs::remove_file(dir.join(bk));
                }
                continue;
            }
            if b.backed_up_gen == b.gen && b.backup.is_some() {
                continue;
            }
            if !made_dir {
                let _ = std::fs::create_dir_all(&dir);
                made_dir = true;
            }
            let name = b.backup.clone().unwrap_or_else(|| {
                let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                format!("{}@{stamp}-{}.txt", sanitize(&b.name()), b.id)
            });
            if std::fs::write(dir.join(&name), b.text_all()).is_ok() {
                b.backup = Some(name);
                b.backed_up_gen = b.gen;
            }
        }
    }

    // ---- session -----------------------------------------------------------------

    pub fn save_session(&mut self) {
        let restore = settings::get().restore_session;
        self.save_session_impl(restore);
    }

    /// Saves every tab, including unsaved text, whatever the restore
    /// setting says (used before an update restart).
    pub fn save_session_forced(&mut self) {
        let was = settings::get().restore_session;
        settings::get_mut().restore_session = true;
        self.save_session_impl(true);
        settings::get_mut().restore_session = was;
    }

    fn save_session_impl(&mut self, restore: bool) {
        if self.no_session {
            return;
        }
        self.write_backups();
        let mut out = String::from("# Slate session\n");
        out += &format!("split={}\nactive_view={}\nsplit_frac={}\n", self.split as u8, self.av, self.split_frac);
        if restore {
            for v in 0..2 {
                for (i, t) in self.views[v].tabs.iter().enumerate() {
                    let b = &self.bufs[self.bi(t.buf)];
                    if Some(b.id) == self.results_buf || (b.path.is_none() && b.backup.is_none()) {
                        continue;
                    }
                    let p = b.primary();
                    out += "[tab]\n";
                    out += &format!("view={v}\nactive={}\n", (i == self.views[v].active) as u8);
                    if let Some(path) = &b.path {
                        out += &format!("path={}\n", path.display());
                    }
                    if let Some(bk) = &b.backup {
                        out += &format!("backup={bk}\nuntitled={}\n", b.untitled);
                    }
                    out += &format!("anchor={}\ncaret={}\ntop={}\n", p.anchor, p.head, t.top);
                    out += &format!("lang={}\nenc={}\neol={}\n", LANGS[b.lang].name, b.enc.name(), b.eol.name());
                    if b.grid.is_some() {
                        out += "grid=1\n";
                    } else if b.path.as_deref().map_or(false, crate::csv::is_table_path) {
                        out += "grid=0\n";
                    }
                    if !b.bookmarks.is_empty() {
                        out += &format!("bookmarks={}\n", b.bookmarks.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","));
                    }
                }
            }
        }
        let _ = std::fs::create_dir_all(settings::dir());
        let _ = std::fs::write(settings::dir().join("session.ini"), out);
        // Backups not referenced by any buffer are stale.
        let keep: Vec<String> = self.bufs.iter().filter_map(|b| b.backup.clone()).collect();
        if let Ok(rd) = std::fs::read_dir(settings::backup_dir()) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if !keep.contains(&n) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    pub fn restore_session(&mut self) {
        let text = std::fs::read_to_string(settings::dir().join("session.ini")).unwrap_or_default();
        let mut tabs: Vec<HashMap<String, String>> = Vec::new();
        let mut split = false;
        let mut av = 0;
        for line in text.lines() {
            if line == "[tab]" {
                tabs.push(HashMap::new());
                continue;
            }
            for (k, v) in settings::parse(line) {
                match (tabs.last_mut(), k) {
                    (None, "split") => split = v == "1",
                    (None, "active_view") => av = v.parse().unwrap_or(0),
                    (None, "split_frac") => self.split_frac = v.parse().unwrap_or(0.5f32).clamp(0.15, 0.85),
                    (Some(t), _) => {
                        t.insert(k.to_string(), v.to_string());
                    }
                    _ => {}
                }
            }
        }
        let mut actives = [None, None];
        for t in tabs {
            let v: usize = t.get("view").and_then(|x| x.parse().ok()).unwrap_or(0).min(1);
            let path = t.get("path").cloned();
            let backup = t.get("backup").cloned();
            let id = if let Some(bk) = &backup {
                let Ok(text) = std::fs::read(settings::backup_dir().join(bk)) else { continue };
                let id = self.new_buffer(text);
                let b = self.bufs.last_mut().unwrap();
                b.backup = Some(bk.clone());
                b.mark_unsaved();
                b.untitled = t.get("untitled").and_then(|x| x.parse().ok()).unwrap_or(0);
                if let Some(p) = &path {
                    b.path = Some(PathBuf::from(p));
                    b.mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok();
                }
                self.next_untitled = self.next_untitled.max(b.untitled + 1);
                id
            } else if let Some(p) = &path {
                let Ok(raw) = crate::doc::read_for_doc(Path::new(p)) else { continue };
                let force = t.get("enc").and_then(|e| Enc::from_name(e));
                let detected = enc::detect(&raw);
                let d = enc::decode(raw, force.filter(|e| *e == detected || *e == Enc::Ansi));
                let id = self.new_buffer(d.text);
                let b = self.bufs.last_mut().unwrap();
                b.enc = d.enc;
                b.eol = d.eol;
                b.mixed_eol = d.mixed_eol;
                b.path = Some(PathBuf::from(p));
                b.mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok();
                id
            } else {
                continue;
            };
            let i = self.bi(id);
            let b = &mut self.bufs[i];
            if let Some(l) = t.get("lang").and_then(|n| LANGS.iter().position(|x| x.name == n)) {
                b.lang = l;
            } else if let Some(p) = &b.path {
                b.lang = syntax::lang_for_path(&p.to_string_lossy());
            }
            if let Some(e) = t.get("eol").and_then(|e| Eol::from_name(e)) {
                b.eol = e;
            }
            if let Some(e) = t.get("enc").and_then(|e| Enc::from_name(e)) {
                b.enc = e;
            }
            let n = b.doc.len();
            let caret = t.get("caret").and_then(|x| x.parse().ok()).unwrap_or(0usize).min(n);
            let anchor = t.get("anchor").and_then(|x| x.parse().ok()).unwrap_or(caret).min(n);
            b.sels = vec![Sel { anchor, head: caret }];
            if let Some(bm) = t.get("bookmarks") {
                let lines = b.doc.line_count();
                b.bookmarks = bm.split(',').filter_map(|x| x.parse().ok()).filter(|&x: &usize| x < lines).collect();
            }
            let top = t.get("top").and_then(|x| x.parse().ok()).unwrap_or(0usize).min(b.doc.line_count() - 1);
            match t.get("grid").map(|s| s.as_str()) {
                Some("1") => self.enable_grid(i),
                Some(_) => {}
                None => self.auto_grid(id),
            }
            self.views[v].tabs.push(Tab { buf: id, top, sub: 0, x: 0.0 });
            if t.get("active").map(|x| x == "1").unwrap_or(false) {
                actives[v] = Some(self.views[v].tabs.len() - 1);
            }
        }
        for v in 0..2 {
            self.views[v].active = actives[v].unwrap_or(0).min(self.views[v].tabs.len().saturating_sub(1));
        }
        if self.views[0].tabs.is_empty() && !self.views[1].tabs.is_empty() {
            self.views.swap(0, 1);
        }
        self.split = split && !self.views[1].tabs.is_empty();
        if !self.split {
            let moved: Vec<Tab> = self.views[1].tabs.drain(..).collect();
            self.views[0].tabs.extend(moved);
        }
        self.av = if self.split { av.min(1) } else { 0 };
    }

    pub fn ensure_tab(&mut self) {
        if self.views[0].tabs.is_empty() {
            self.av = 0;
            self.new_file();
        }
    }

    /// Reloads files changed on disk (silently when the buffer is clean).
    pub fn check_disk(&mut self) {
        let mut ask = Vec::new();
        for i in 0..self.bufs.len() {
            let b = &self.bufs[i];
            let Some(p) = &b.path else { continue };
            let Ok(m) = std::fs::metadata(p).and_then(|m| m.modified()) else { continue };
            if b.mtime.is_some() && b.mtime != Some(m) {
                if b.dirty() {
                    ask.push(b.id);
                } else {
                    let id = b.id;
                    self.reload(id, None);
                    let name = self.bufs[self.bi(id)].name();
                    self.flash(&format!("Reloaded {name} (changed on disk)"));
                }
            }
        }
        for id in ask {
            let i = self.bi(id);
            let name = self.bufs[i].path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
            self.bufs[i].mtime = self.bufs[i].path.as_ref().and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
            if sys::ask_yes_no(self.hwnd, &format!("{name}\n\nThis file was changed by another program. Reload it and lose your unsaved changes?")) {
                self.reload(id, None);
            }
        }
    }

    pub fn reload(&mut self, id: u32, force: Option<Enc>) {
        let i = self.bi(id);
        let Some(p) = self.bufs[i].path.clone() else { return };
        let Ok(raw) = crate::doc::read_for_doc(&p) else { return };
        let d = enc::decode(raw, force);
        let grid = self.bufs[i].grid.take();
        let old = &self.bufs[i];
        let caret = old.caret();
        let mut nb = Buffer::new(id, d.text);
        nb.path = Some(p.clone());
        nb.enc = d.enc;
        nb.eol = d.eol;
        nb.mixed_eol = d.mixed_eol;
        nb.lang = old.lang;
        nb.untitled = old.untitled;
        nb.bookmarks = old.bookmarks.clone();
        nb.tab_size = old.tab_size;
        nb.mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        let lines = nb.doc.line_count();
        nb.bookmarks.retain(|&l| l < lines);
        nb.set_caret(caret);
        // The grid keeps its filters, sort and widths over the new text.
        if let Some(mut g) = grid {
            g.edit = None;
            g.reindex(&mut nb.doc, nb.gen);
            nb.grid = Some(g);
        }
        self.bufs[i] = nb;
        self.after_switch();
    }

    // ---- find ------------------------------------------------------------------------

    pub fn open_find(&mut self, replace: bool, fif: bool) {
        let seed = {
            let b = self.cur_ref();
            let p = b.primary();
            if !p.is_empty() && p.hi() - p.lo() < 300 {
                let t = b.doc.text(p.lo(), p.hi());
                (!t.contains(&b'\n')).then(|| String::from_utf8_lossy(&t).into_owned())
            } else {
                let (a, z) = b.word_at(p.head);
                (z > a && z - a < 100).then(|| String::from_utf8_lossy(&b.doc.text(a, z)).into_owned())
            }
        };
        let dir = self.cur_ref().path.as_ref().and_then(|p| p.parent()).map(|p| p.display().to_string()).unwrap_or_default();
        if self.find.is_none() {
            self.find_since = Some(Instant::now());
        }
        let fb = self.find.get_or_insert_with(|| FindBar {
            replace: false,
            fif: false,
            fields: Default::default(),
            focus: 0,
            case: false,
            word: false,
            regex: false,
            count: None,
            err: None,
        });
        fb.replace = replace;
        fb.fif = fif;
        fb.focus = 0;
        self.in_find = true;
        if let Some(s) = seed {
            fb.fields[0].set(&s);
        } else if fb.fields[0].text.is_empty() && !self.last_find.pat.is_empty() {
            fb.fields[0].set(&self.last_find.pat.clone());
        }
        fb.fields[0].anchor = 0;
        fb.fields[0].caret = fb.fields[0].text.len();
        if fb.fields[1].text.is_empty() {
            let r = self.repl_text.clone();
            fb.fields[1].set(&r);
        }
        if fif {
            if fb.fields[2].text.is_empty() {
                fb.fields[2].set(&dir);
            }
            if fb.fields[3].text.is_empty() {
                fb.fields[3].set("*.*");
            }
        }
        self.palette = None;
        self.recount();
        self.redraw();
    }

    /// Recomputes the match count shown in the find bar.
    pub fn recount(&mut self) {
        let Some(fb) = &self.find else { return };
        let q = fb.query();
        self.last_find = q.clone();
        let (count, err) = match q.compile() {
            Ok(m) => {
                let text = String::from_utf8_lossy(self.cur().doc.gap.contiguous());
                (Some(m.find_all(&text, 100_000).len()), None)
            }
            Err(e) if e.is_empty() => (None, None),
            Err(e) => (None, Some(e)),
        };
        let fb = self.find.as_mut().unwrap();
        fb.count = count;
        fb.err = err;
    }

    pub fn find_step(&mut self, forward: bool) {
        let q = match &self.find {
            Some(fb) => fb.query(),
            None => self.last_find.clone(),
        };
        if q.pat.is_empty() {
            self.open_find(false, false);
            return;
        }
        let Ok(m) = q.compile() else { return };
        self.last_find = q;
        let b = self.cur();
        let p = b.primary();
        // Borrowed, not copied: valid UTF-8 needs no allocation.
        let hit = {
            let text = String::from_utf8_lossy(b.doc.gap.contiguous());
            if forward {
                m.find_from(&text, p.hi()).or_else(|| m.find_from(&text, 0))
            } else {
                m.find_before(&text, p.lo()).or_else(|| m.find_before(&text, text.len() + 1))
            }
        };
        match hit {
            Some((a, z)) => {
                b.sels = vec![Sel { anchor: a, head: z }];
                b.seal = true;
                self.reveal_caret();
            }
            None => self.flash("No matches"),
        }
        self.redraw();
    }

    pub fn replace_one(&mut self) {
        let Some(fb) = &self.find else { return };
        let q = fb.query();
        let repl = fb.fields[1].text.clone();
        self.repl_text = repl.clone();
        let Ok(m) = q.compile() else { return };
        let b = self.cur();
        let p = b.primary();
        let r = {
            let text = String::from_utf8_lossy(b.doc.gap.contiguous());
            (!p.is_empty() && m.find_from(&text, p.lo()) == Some((p.lo(), p.hi())))
                .then(|| m.expand(&q, &text, (p.lo(), p.hi()), &repl))
        };
        if let Some(r) = r {
            b.apply(vec![(p.lo(), p.hi(), r.into_bytes())], EditKind::Other);
            self.edited();
        }
        self.find_step(true);
        self.recount();
    }

    pub fn replace_all(&mut self) {
        let Some(fb) = &self.find else { return };
        let q = fb.query();
        let repl = fb.fields[1].text.clone();
        self.repl_text = repl.clone();
        let Ok(m) = q.compile() else { return };
        let b = self.cur();
        let ch: Vec<_> = {
            let text = String::from_utf8_lossy(b.doc.gap.contiguous());
            m.find_all(&text, usize::MAX).into_iter().map(|r| (r.0, r.1, m.expand(&q, &text, r, &repl).into_bytes())).collect()
        };
        let n = ch.len();
        b.apply(ch, EditKind::Other);
        self.edited();
        self.recount();
        self.flash(&format!("Replaced {n} occurrence{}", if n == 1 { "" } else { "s" }));
    }

    pub fn run_fif(&mut self) {
        let Some(fb) = &self.find else { return };
        let req = search::FifRequest { query: fb.query(), dir: fb.fields[2].text.trim().to_string(), filter: fb.fields[3].text.clone() };
        if req.query.pat.is_empty() || req.dir.is_empty() {
            return;
        }
        self.flash(&format!("Searching {} …", req.dir));
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let out = search::find_in_files(&req);
            let p = Box::into_raw(Box::new(out));
            unsafe {
                let _ = PostMessageW(HWND(hwnd as *mut _), WM_FIF_DONE, WPARAM(0), LPARAM(p as isize));
            }
        });
    }

    pub fn fif_done(&mut self, text: String) {
        let summary = text.lines().next().unwrap_or("").to_string();
        let id = match self.results_buf.filter(|id| self.bufs.iter().any(|b| b.id == *id)) {
            Some(id) => {
                let i = self.bi(id);
                let b = &mut self.bufs[i];
                let mut t = text.into_bytes();
                t.extend_from_slice(b"\n");
                b.apply(vec![(0, 0, t)], EditKind::Other);
                b.set_caret(0);
                b.mark_saved();
                id
            }
            None => {
                let id = self.new_buffer(text.into_bytes());
                let b = self.bufs.last_mut().unwrap();
                b.label = Some("Search results".into());
                b.lang = 0;
                self.results_buf = Some(id);
                id
            }
        };
        // Keys now go to the results so Enter opens the hit under the caret.
        self.in_find = false;
        match self.find_open_id(id) {
            Some((v, i)) => {
                self.views[v].active = i;
                self.av = v;
                self.tab_mut(v).top = 0;
            }
            None => self.add_tab(self.av, id),
        }
        self.flash(&summary);
        self.after_switch();
    }

    pub fn find_open_id(&self, id: u32) -> Option<(usize, usize)> {
        (0..2).find_map(|v| self.views[v].tabs.iter().position(|t| t.buf == id).map(|i| (v, i)))
    }

    /// Opens the file/line under the caret of the find-results tab.
    pub fn follow_result(&mut self) -> bool {
        if Some(self.cur_ref().id) != self.results_buf {
            return false;
        }
        let b = self.cur_ref();
        let line = b.doc.line_of(b.caret());
        let from = line.saturating_sub(5000);
        let lines: Vec<String> = (from..=line).map(|l| String::from_utf8_lossy(&b.doc.text(b.doc.line_start(l), b.doc.line_end(l))).into_owned()).collect();
        let Some((path, n)) = search::result_target(&lines, lines.len() - 1) else { return false };
        let q = self.last_find.clone();
        if self.split {
            self.av = 1 - self.av;
        }
        if self.open_path(&path) {
            let b = self.cur();
            let l = (n - 1).min(b.doc.line_count() - 1);
            let s = b.doc.line_start(l);
            let e = b.doc.line_end(l);
            let text = String::from_utf8_lossy(&b.doc.text(s, e)).into_owned();
            let hit = q.compile().ok().and_then(|m| m.find_from(&text, 0));
            b.sels = vec![match hit {
                Some((a, z)) => Sel { anchor: s + a, head: s + z },
                None => Sel::caret(s),
            }];
            self.reveal_caret();
            self.center_caret();
        }
        true
    }

    // ---- palette ------------------------------------------------------------------------

    pub fn open_palette(&mut self, mode: PalMode, title: &str, items: Vec<PalItem>, initial: &str) {
        if self.palette.is_none() {
            self.pal_since = Some(Instant::now());
        }
        let mut p = Palette { mode, title: title.to_string(), input: Input::default(), items, shown: Vec::new(), sel: 0, scroll: 0 };
        p.input.set(initial);
        self.palette = Some(p);
        self.filter_palette();
        self.redraw();
    }

    pub fn filter_palette(&mut self) {
        let Some(p) = self.palette.as_mut() else { return };
        let q = p.input.text.to_lowercase();
        let mut scored: Vec<(i32, usize)> = Vec::new();
        for (i, it) in p.items.iter().enumerate() {
            if let Some(s) = fuzzy(&q, &it.label.to_lowercase()) {
                scored.push((s, i));
            }
        }
        if !q.is_empty() {
            scored.sort_by_key(|x| -x.0);
        }
        p.shown = scored.into_iter().map(|x| x.1).collect();
        p.sel = 0;
        p.scroll = 0;
    }

    pub fn command_palette(&mut self) {
        let items = COMMANDS
            .iter()
            .map(|(c, l, k)| PalItem { label: l.to_string(), hint: k.to_string(), act: PalAct::Cmd(*c) })
            .collect();
        self.open_palette(PalMode::Commands, "", items, "");
    }

    pub fn palette_accept(&mut self) {
        let Some(p) = self.palette.take() else { return };
        if p.mode == PalMode::Goto {
            let t = p.input.text.trim_start_matches(':').trim();
            let mut it = t.split([':', ',']);
            let line: usize = it.next().and_then(|x| x.trim().parse().ok()).unwrap_or(0);
            let col: usize = it.next().and_then(|x| x.trim().parse().ok()).unwrap_or(1);
            if line > 0 {
                self.goto_line(line - 1, col.saturating_sub(1));
            }
            self.redraw();
            return;
        }
        let Some(&i) = p.shown.get(p.sel) else {
            self.redraw();
            return;
        };
        let act = p.items[i].act.clone();
        self.redraw();
        match act {
            PalAct::Cmd(c) => self.exec(c),
            PalAct::Tab(v, i) => {
                if i < self.views[v].tabs.len() {
                    self.views[v].active = i;
                    self.av = v;
                    self.after_switch();
                }
            }
            PalAct::Open(path) => {
                self.open_path(&path);
            }
            PalAct::Lang(l) => {
                self.cur().set_lang(l);
            }
            PalAct::Enc(e) => {
                let b = self.cur();
                if b.enc != e {
                    b.enc = e;
                    b.mark_unsaved();
                    self.flash(&format!("Encoding set to {} (applies on save)", e.name()));
                    self.update_title();
                }
            }
            PalAct::Reopen(e) => {
                let id = self.cur_ref().id;
                if self.cur_ref().dirty() && !sys::ask_yes_no(self.hwnd, "Reopening discards your unsaved changes. Continue?") {
                    return;
                }
                self.reload(id, Some(e));
            }
            PalAct::Eol(e) => {
                let b = self.cur();
                if b.eol != e || b.mixed_eol {
                    b.eol = e;
                    b.mixed_eol = false;
                    b.mark_unsaved();
                    self.update_title();
                }
            }
            PalAct::Accent(i) => {
                settings::get_mut().accent = i;
                self.gfx.accent = i;
                let _ = self.gfx.make_brushes();
                self.cache.clear();
            }
            PalAct::Delim(d) => self.grid_set_delim(d),
            PalAct::Compare(id) => self.compare_buf(id),
            PalAct::TabSize(n) => {
                settings::get_mut().tab_size = n;
                for b in self.bufs.iter_mut() {
                    b.tab_size = n;
                }
                self.rebuild_fonts();
            }
        }
        self.redraw();
    }

    pub fn goto_line(&mut self, line: usize, col: usize) {
        let b = self.cur();
        let l = line.min(b.doc.line_count() - 1);
        b.reveal(l);
        let p = (b.doc.line_start(l) + col).min(b.doc.line_end(l));
        b.set_caret(p);
        self.reveal_caret();
        self.center_caret();
    }

    // ---- fonts / layout cache -------------------------------------------------------------

    pub fn rebuild_fonts(&mut self) {
        let (font, size, tab) = {
            let s = settings::get();
            (s.font.clone(), (s.font_size + s.zoom as f32).max(6.0), s.tab_size)
        };
        let _ = self.gfx.make_fonts(&font, size, tab);
        self.cache.clear();
        self.font_gen += 1;
        self.redraw();
    }

    // ---- commands -----------------------------------------------------------------------------

    pub fn exec(&mut self, c: Cmd) {
        let hwnd = self.hwnd;
        let mut changed = true;
        // A tab that is still loading holds an empty placeholder: nothing
        // may edit or save it.
        let safe_while_loading = matches!(
            c,
            Cmd::New | Cmd::Open | Cmd::Close | Cmd::CloseAll | Cmd::CloseOthers | Cmd::ReopenClosed | Cmd::Recent
                | Cmd::SwitchFile | Cmd::Reveal | Cmd::CopyPath | Cmd::Exit | Cmd::Palette | Cmd::ZoomIn | Cmd::ZoomOut
                | Cmd::ZoomReset | Cmd::Wrap | Cmd::Whitespace | Cmd::Theme | Cmd::MoveOther | Cmd::CloneOther
                | Cmd::FocusOther | Cmd::CloseSplit | Cmd::NextTab | Cmd::PrevTab | Cmd::ToggleSession | Cmd::About
                | Cmd::CheckUpdates | Cmd::InstallUpdate | Cmd::AutoUpdate | Cmd::SaveAll | Cmd::FindInFiles
                | Cmd::MacroRecord | Cmd::UseTabs | Cmd::TabSize | Cmd::Accent | Cmd::Animations
        );
        if !self.bufs.is_empty() && self.cur_ref().loading.is_some() && !safe_while_loading {
            self.flash("Still loading this file…");
            return;
        }
        let grid_cmd = matches!(c, Cmd::GridHeader | Cmd::GridDelim | Cmd::GridClear | Cmd::GridFit | Cmd::GridCopyHeaders);
        if self.grid_active() {
            self.grid_commit();
            match c {
                Cmd::Copy => return self.grid_copy(false),
                Cmd::GridCopyHeaders => return self.grid_copy(true),
                Cmd::Cut => {
                    self.grid_copy(false);
                    self.grid_clear(self.av);
                    return;
                }
                Cmd::Paste => return self.grid_paste(),
                Cmd::SelectAll => {
                    self.grid_select_all(self.av);
                    return self.redraw();
                }
                Cmd::Find | Cmd::Replace => {
                    self.grid_focus_filter();
                    return self.redraw();
                }
                // Commands that move or edit the text caret have no meaning here.
                _ if !(safe_while_loading
                    || grid_cmd
                    || matches!(c, Cmd::Grid | Cmd::GridDefault | Cmd::Save | Cmd::SaveAs | Cmd::Reload | Cmd::Undo | Cmd::Redo
                        | Cmd::Language | Cmd::Encoding | Cmd::ReopenEncoding | Cmd::LineEnding)) =>
                {
                    self.flash("Not available in the grid \u{2013} Ctrl+Shift+G switches to the text view");
                    return;
                }
                _ => {}
            }
        } else if grid_cmd {
            self.flash("Open the grid first \u{2013} Ctrl+Shift+G");
            return;
        }
        if self.in_compare() {
            let edits = matches!(
                c,
                Cmd::Save | Cmd::SaveAs | Cmd::Cut | Cmd::Paste | Cmd::Undo | Cmd::Redo | Cmd::Replace | Cmd::ToggleComment
                    | Cmd::Comment | Cmd::Uncomment | Cmd::Duplicate | Cmd::DeleteLine | Cmd::MoveUp | Cmd::MoveDown
                    | Cmd::Upper | Cmd::Lower | Cmd::TitleCase | Cmd::SortAsc | Cmd::SortDesc | Cmd::Unique | Cmd::Reverse
                    | Cmd::Trim | Cmd::Join | Cmd::TabsToSpaces | Cmd::SpacesToTabs | Cmd::MacroPlay | Cmd::Encoding
                    | Cmd::ReopenEncoding | Cmd::LineEnding | Cmd::Grid | Cmd::ToggleFold | Cmd::FoldAll
            );
            if edits {
                self.flash("The compare view is read-only \u{2013} edit the file itself, then F5 refreshes");
                return;
            }
            if c == Cmd::Reload {
                return self.cmp_refresh();
            }
        }
        match c {
            Cmd::Compare => {
                self.compare_with();
                changed = false;
            }
            Cmd::CompareFile => {
                self.compare_file();
                changed = false;
            }
            Cmd::CompareSaved => {
                self.compare_saved();
                changed = false;
            }
            Cmd::CompareNext | Cmd::ComparePrev => {
                self.cmp_step(c == Cmd::CompareNext);
                changed = false;
            }
            Cmd::CompareRefresh => {
                if self.has_compare() {
                    self.cmp_refresh();
                } else {
                    self.flash("Open a compare first \u{2013} Ctrl+Alt+C");
                }
                changed = false;
            }
            Cmd::CompareSwap => {
                self.cmp_swap();
                changed = false;
            }
            Cmd::CompareWs | Cmd::CompareCase => {
                self.cmp_toggle_opt(c == Cmd::CompareCase);
                changed = false;
            }
            Cmd::CompareClose => {
                self.close_compare();
                changed = false;
            }
            Cmd::Grid => {
                self.toggle_grid();
                changed = false;
            }
            Cmd::GridHeader => {
                self.grid_toggle_header();
                changed = false;
            }
            Cmd::GridDelim => {
                self.grid_delim_palette();
                changed = false;
            }
            Cmd::GridClear => {
                self.grid_clear_filters();
                changed = false;
            }
            Cmd::GridFit => {
                self.grid_fit_all();
                changed = false;
            }
            Cmd::GridCopyHeaders => changed = false,
            Cmd::GridDefault => {
                let on = {
                    let mut s = settings::get_mut();
                    s.csv_grid = !s.csv_grid;
                    s.csv_grid
                };
                self.flash(if on { "CSV files open as a grid" } else { "CSV files open as text" });
                changed = false;
            }
            Cmd::New => self.new_file(),
            Cmd::Open => self.open_dialog(),
            Cmd::Save => {
                let id = self.cur_ref().id;
                self.save_buf(id, false);
            }
            Cmd::SaveAs => {
                let id = self.cur_ref().id;
                self.save_buf(id, true);
            }
            Cmd::SaveAll => {
                let ids: Vec<u32> = self.bufs.iter().filter(|b| b.dirty() && b.loading.is_none() && Some(b.id) != self.results_buf).map(|b| b.id).collect();
                for id in ids {
                    if let Some((v, i)) = self.find_open_id(id) {
                        self.views[v].active = i;
                        self.av = v;
                    }
                    if !self.save_buf(id, false) {
                        break;
                    }
                }
            }
            Cmd::Close => {
                let (v, a) = (self.av, self.views[self.av].active);
                self.close_tab(v, a);
            }
            Cmd::CloseAll => self.close_where(None),
            Cmd::CloseOthers => self.close_where(Some(self.views[self.av].active)),
            Cmd::ReopenClosed => {
                if let Some(p) = self.closed.pop() {
                    self.open_path(&p);
                }
            }
            Cmd::Recent => {
                let items = settings::get()
                    .recent
                    .iter()
                    .map(|r| {
                        let name = Path::new(r).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        PalItem { label: name, hint: r.clone(), act: PalAct::Open(r.clone()) }
                    })
                    .collect();
                self.open_palette(PalMode::List, "Open recent", items, "");
            }
            Cmd::SwitchFile => {
                let mut items = Vec::new();
                for v in 0..2 {
                    for (i, t) in self.views[v].tabs.iter().enumerate() {
                        let b = &self.bufs[self.bi(t.buf)];
                        let hint = b.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                        items.push(PalItem { label: b.name(), hint, act: PalAct::Tab(v, i) });
                    }
                }
                self.open_palette(PalMode::List, "Go to tab", items, "");
            }
            Cmd::Reload => {
                let id = self.cur_ref().id;
                if !self.cur_ref().dirty() || sys::ask_yes_no(hwnd, "Reload from disk and lose your unsaved changes?") {
                    self.reload(id, None);
                }
            }
            Cmd::Reveal => {
                if let Some(p) = self.cur_ref().path.clone() {
                    let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", p.display())).spawn();
                }
            }
            Cmd::CopyPath => {
                if let Some(p) = self.cur_ref().path.clone() {
                    sys::set_clipboard(hwnd, p.display().to_string().as_bytes(), false);
                    self.flash("Path copied");
                }
            }
            Cmd::Exit => unsafe {
                let _ = PostMessageW(hwnd, windows::Win32::UI::WindowsAndMessaging::WM_CLOSE, WPARAM(0), LPARAM(0));
            },
            Cmd::Undo => {
                self.cur().undo();
            }
            Cmd::Redo => {
                self.cur().redo();
            }
            Cmd::Cut | Cmd::Copy => {
                let b = self.cur();
                let column = b.sels.len() > 1;
                let mut t = edit::selected_text(b);
                let whole_line = b.sels.iter().all(|s| s.is_empty());
                if whole_line {
                    let l = b.doc.line_of(b.caret());
                    t = b.doc.text(b.doc.line_start(l), b.doc.line_end(l));
                    t.push(b'\n');
                }
                if !column && b.path.as_deref().map_or(false, crate::csv::is_table_path) {
                    // CSV text keeps its commas for editors; spreadsheets get columns.
                    let d = crate::csv::sniff(&b.doc.text(0, b.doc.len().min(1 << 16)));
                    let rows = crate::csv::parse_rows(&t, d);
                    sys::set_clipboard_with(hwnd, &String::from_utf8_lossy(&t).replace('\n', "\r\n"), &sys::table_formats(&rows));
                } else {
                    sys::set_clipboard(hwnd, &t, column);
                }
                if c == Cmd::Cut {
                    let b = self.cur();
                    if whole_line {
                        edit::delete_lines(b);
                    } else {
                        edit::insert(b, b"", EditKind::Other);
                    }
                } else {
                    changed = false;
                }
            }
            Cmd::Paste => {
                if let Some((t, column)) = sys::get_clipboard(hwnd) {
                    let b = self.cur();
                    let parts: Vec<Vec<u8>> = t.strip_suffix(b"\n").unwrap_or(&t).split(|&c| c == b'\n').map(|s| s.to_vec()).collect();
                    if b.sels.len() > 1 && parts.len() == b.sels.len() {
                        edit::insert_each(b, &parts);
                    } else if column && b.sels.len() == 1 && parts.len() > 1 {
                        // Rectangular paste: one row per line from the caret column.
                        let p = b.caret();
                        let (l0, col) = (b.doc.line_of(p), b.display_col(p));
                        while b.doc.line_count() < l0 + parts.len() {
                            let n = b.doc.len();
                            b.apply(vec![(n, n, b"\n".to_vec())], EditKind::Other);
                        }
                        let mut ch = Vec::new();
                        for (k, part) in parts.iter().enumerate() {
                            let l = l0 + k;
                            let at = b.pos_at_col(l, col);
                            let have = b.display_col(at);
                            let mut t = vec![b' '; col.saturating_sub(have)];
                            t.extend_from_slice(part);
                            ch.push((at, at, t));
                        }
                        b.apply(ch, EditKind::Other);
                    } else {
                        edit::insert(b, &t, EditKind::Other);
                    }
                }
            }
            Cmd::SelectAll => edit::select_all(self.cur()),
            Cmd::SelectLine => edit::select_line(self.cur()),
            Cmd::AddNext => edit::add_next_occurrence(self.cur()),
            Cmd::ColumnUp | Cmd::ColumnDown => {
                let b = self.cur();
                let p = b.primary().head;
                let (l, col) = (b.doc.line_of(p), b.want_col.unwrap_or_else(|| b.display_col(p)));
                let nl = if c == Cmd::ColumnUp { b.prev_visible(l) } else { b.next_visible(l) };
                if let Some(nl) = nl {
                    let q = b.pos_at_col(nl, col);
                    b.sels.push(Sel::caret(q));
                    b.normalize();
                    b.want_col = Some(col);
                }
            }
            Cmd::Find => self.open_find(false, false),
            Cmd::Replace => self.open_find(true, false),
            Cmd::FindInFiles => self.open_find(false, true),
            Cmd::FindNext => self.find_step(true),
            Cmd::FindPrev => self.find_step(false),
            Cmd::GotoLine => {
                let n = self.cur_ref().doc.line_count();
                self.open_palette(PalMode::Goto, &format!("Go to line (1 – {n}), optionally :column"), Vec::new(), ":");
            }
            Cmd::GotoBrace => {
                let b = self.cur();
                if let Some((_, q)) = edit::match_bracket(b, b.caret()) {
                    b.set_caret(q);
                }
            }
            Cmd::Palette => self.command_palette(),
            Cmd::ToggleComment => edit::toggle_comment(self.cur()),
            Cmd::Comment | Cmd::Uncomment => {
                // Toggle only when it moves in the requested direction.
                let b = self.cur();
                let gen = b.gen;
                edit::toggle_comment(b);
                let undo_needed = {
                    let lang = &LANGS[b.lang];
                    let prefix = lang.line.first().map(|p| p.trim_end()).unwrap_or("");
                    let l = b.doc.line_of(b.primary().lo());
                    let s = b.doc.line_start(l) + b.line_indent(l).0;
                    let now_commented = !prefix.is_empty() && b.doc.text(s, (s + prefix.len()).min(b.doc.len())) == prefix.as_bytes();
                    b.gen != gen && now_commented != (c == Cmd::Comment)
                };
                if undo_needed {
                    b.undo();
                }
            }
            Cmd::Duplicate => edit::duplicate(self.cur()),
            Cmd::DeleteLine => edit::delete_lines(self.cur()),
            Cmd::MoveUp => edit::move_lines(self.cur(), true),
            Cmd::MoveDown => edit::move_lines(self.cur(), false),
            Cmd::Join => edit::join_lines(self.cur()),
            Cmd::Upper => edit::transform_sel(self.cur(), |s| s.to_uppercase()),
            Cmd::Lower => edit::transform_sel(self.cur(), |s| s.to_lowercase()),
            Cmd::TitleCase => edit::transform_sel(self.cur(), title_case),
            Cmd::SortAsc => edit::transform_lines(self.cur(), |mut v| {
                v.sort_by_key(|a| a.to_lowercase());
                v
            }),
            Cmd::SortDesc => edit::transform_lines(self.cur(), |mut v| {
                v.sort_by_key(|a| a.to_lowercase());
                v.reverse();
                v
            }),
            Cmd::Unique => edit::transform_lines(self.cur(), |v| {
                let mut seen = std::collections::HashSet::new();
                v.into_iter().filter(|l| seen.insert(l.clone())).collect()
            }),
            Cmd::Reverse => edit::transform_lines(self.cur(), |mut v| {
                v.reverse();
                v
            }),
            Cmd::Trim => edit::trim_trailing(self.cur()),
            Cmd::TabsToSpaces => edit::tabs_spaces(self.cur(), true),
            Cmd::SpacesToTabs => edit::tabs_spaces(self.cur(), false),
            Cmd::ToggleBookmark => {
                let b = self.cur();
                let l = b.doc.line_of(b.caret());
                b.toggle_bookmark(l);
                changed = false;
            }
            Cmd::NextBookmark | Cmd::PrevBookmark => {
                let b = self.cur();
                let l = b.doc.line_of(b.caret());
                let t = if c == Cmd::NextBookmark {
                    b.bookmarks.iter().find(|&&x| x > l).or(b.bookmarks.first()).copied()
                } else {
                    b.bookmarks.iter().rev().find(|&&x| x < l).or(b.bookmarks.last()).copied()
                };
                if let Some(t) = t {
                    self.goto_line(t, 0);
                }
                changed = false;
            }
            Cmd::ClearBookmarks => {
                self.cur().bookmarks.clear();
                changed = false;
            }
            Cmd::ToggleFold => {
                let b = self.cur();
                let l = b.doc.line_of(b.caret());
                // Fold the block the caret is in when its own line heads none.
                let target = if b.is_folded(l) || b.fold_end(l).is_some() {
                    Some(l)
                } else {
                    (0..l).rev().find(|&h| b.fold_end(h).map(|e| e >= l).unwrap_or(false))
                };
                if let Some(h) = target {
                    b.toggle_fold(h);
                }
                changed = false;
            }
            Cmd::FoldAll => {
                self.cur().fold_all();
                changed = false;
            }
            Cmd::UnfoldAll => {
                self.cur().folds.clear();
                changed = false;
            }
            Cmd::ZoomIn | Cmd::ZoomOut | Cmd::ZoomReset => {
                {
                    let mut s = settings::get_mut();
                    s.zoom = match c {
                        Cmd::ZoomIn => (s.zoom + 1).min(30),
                        Cmd::ZoomOut => (s.zoom - 1).max(-8),
                        _ => 0,
                    };
                }
                self.rebuild_fonts();
                changed = false;
            }
            Cmd::Wrap => {
                let on = {
                    let mut s = settings::get_mut();
                    s.wrap = !s.wrap;
                    s.wrap
                };
                for v in 0..2 {
                    for t in self.views[v].tabs.iter_mut() {
                        t.sub = 0;
                        t.x = 0.0;
                    }
                }
                self.flash(if on { "Word wrap on" } else { "Word wrap off" });
                changed = false;
            }
            Cmd::Whitespace => {
                let mut s = settings::get_mut();
                s.show_ws = !s.show_ws;
                changed = false;
            }
            Cmd::Theme => {
                let dark = {
                    let mut s = settings::get_mut();
                    s.dark = !s.dark;
                    s.dark
                };
                self.gfx.dark = dark;
                let _ = self.gfx.make_brushes();
                self.cache.clear();
                changed = false;
            }
            Cmd::MoveOther | Cmd::CloneOther => {
                let v = self.av;
                let o = 1 - v;
                let id = self.tab(v).buf;
                if let Some(i) = self.views[o].tabs.iter().position(|t| t.buf == id) {
                    self.views[o].active = i;
                } else {
                    let top = self.tab(v).top;
                    let at = if self.views[o].tabs.is_empty() { 0 } else { self.views[o].active + 1 };
                    self.views[o].tabs.insert(at, Tab { buf: id, top, sub: 0, x: 0.0 });
                    self.views[o].active = at;
                }
                self.split = true;
                if c == Cmd::MoveOther {
                    let a = self.views[v].active;
                    self.views[v].tabs.remove(a);
                    self.views[v].active = a.min(self.views[v].tabs.len().saturating_sub(1));
                    if self.views[v].tabs.is_empty() {
                        if v == 0 {
                            self.views.swap(0, 1);
                        }
                        self.split = false;
                        self.av = 0;
                    } else {
                        self.av = o;
                    }
                } else {
                    self.av = o;
                }
                changed = false;
            }
            Cmd::FocusOther => {
                if self.split {
                    self.av = 1 - self.av;
                }
                changed = false;
            }
            Cmd::CloseSplit => {
                if self.split {
                    let moved: Vec<Tab> = self.views[1].tabs.drain(..).collect();
                    for t in moved {
                        if !self.views[0].tabs.iter().any(|x| x.buf == t.buf) {
                            self.views[0].tabs.push(t);
                        }
                    }
                    self.split = false;
                    self.av = 0;
                }
                changed = false;
            }
            Cmd::NextTab | Cmd::PrevTab => {
                let vw = &mut self.views[self.av];
                let n = vw.tabs.len();
                vw.active = if c == Cmd::NextTab { (vw.active + 1) % n } else { (vw.active + n - 1) % n };
                changed = false;
            }
            Cmd::MacroRecord => {
                if let Some(m) = self.recording.take() {
                    self.flash(&format!("Macro recorded ({} steps) – Ctrl+Shift+M plays it", m.len()));
                    self.last_macro = m;
                } else {
                    self.recording = Some(Vec::new());
                    self.flash("Recording macro… Ctrl+Shift+R stops");
                }
                changed = false;
            }
            Cmd::MacroPlay => {
                if !self.replaying && self.recording.is_none() {
                    self.replaying = true;
                    let m = self.last_macro.clone();
                    for a in m {
                        match a {
                            Action::Char(ch) => self.on_char(ch),
                            Action::Key(vk, ctrl, shift, alt) => {
                                self.on_key(vk, ctrl, shift, alt);
                            }
                        }
                    }
                    self.replaying = false;
                }
            }
            Cmd::Language => {
                let cur = self.cur_ref().lang;
                let items = LANGS
                    .iter()
                    .enumerate()
                    .map(|(i, l)| PalItem { label: l.name.to_string(), hint: if i == cur { "current".into() } else { l.exts.iter().take(4).map(|e| format!(".{e}")).collect::<Vec<_>>().join(" ") }, act: PalAct::Lang(i) })
                    .collect();
                self.open_palette(PalMode::List, "Language", items, "");
            }
            Cmd::Encoding | Cmd::ReopenEncoding => {
                let cur = self.cur_ref().enc;
                let reopen = c == Cmd::ReopenEncoding;
                let items = Enc::ALL
                    .iter()
                    .map(|e| PalItem { label: e.name().into(), hint: if *e == cur { "current".into() } else { String::new() }, act: if reopen { PalAct::Reopen(*e) } else { PalAct::Enc(*e) } })
                    .collect();
                self.open_palette(PalMode::List, if reopen { "Reopen with encoding" } else { "Save with encoding" }, items, "");
            }
            Cmd::LineEnding => {
                let cur = self.cur_ref().eol;
                let items = Eol::ALL
                    .iter()
                    .map(|e| PalItem {
                        label: format!("{} ({})", e.name(), match e { Eol::Crlf => "Windows", Eol::Lf => "Unix / macOS", Eol::Cr => "classic Mac" }),
                        hint: if *e == cur { "current".into() } else { String::new() },
                        act: PalAct::Eol(*e),
                    })
                    .collect();
                self.open_palette(PalMode::List, "Line endings", items, "");
            }
            Cmd::TabSize => {
                let cur = settings::get().tab_size;
                let items = [2usize, 3, 4, 8]
                    .iter()
                    .map(|n| PalItem { label: format!("Tab width {n}"), hint: if *n == cur { "current".into() } else { String::new() }, act: PalAct::TabSize(*n) })
                    .collect();
                self.open_palette(PalMode::List, "Tab width", items, "");
            }
            Cmd::UseTabs => {
                let t = {
                    let mut s = settings::get_mut();
                    s.use_tabs = !s.use_tabs;
                    s.use_tabs
                };
                self.flash(if t { "Indent with tabs" } else { "Indent with spaces" });
                changed = false;
            }
            Cmd::Overtype => {
                self.overtype = !self.overtype;
                changed = false;
            }
            Cmd::ToggleSession => {
                let on = {
                    let mut s = settings::get_mut();
                    s.restore_session = !s.restore_session;
                    s.restore_session
                };
                self.flash(if on { "Session restore on" } else { "Session restore off" });
                changed = false;
            }
            Cmd::Accent => {
                let cur = settings::get().accent;
                let items = crate::gfx::ACCENTS
                    .iter()
                    .enumerate()
                    .map(|(i, a)| PalItem { label: a.0.to_string(), hint: if i == cur { "current".into() } else { String::new() }, act: PalAct::Accent(i) })
                    .collect();
                self.open_palette(PalMode::List, "Accent colour", items, "");
                changed = false;
            }
            Cmd::Animations => {
                let on = {
                    let mut s = settings::get_mut();
                    s.animate = !s.animate;
                    s.animate
                };
                self.flash(if on { "Animations on" } else { "Animations off" });
                changed = false;
            }
            Cmd::CheckUpdates => {
                if self.updates_enabled() {
                    self.start_check(true);
                }
                changed = false;
            }
            Cmd::InstallUpdate => {
                if self.updates_enabled() {
                    self.install_update();
                }
                changed = false;
            }
            Cmd::AutoUpdate => {
                let on = {
                    let mut s = settings::get_mut();
                    s.auto_update = !s.auto_update;
                    s.auto_update
                };
                if on {
                    self.schedule_update_checks();
                } else {
                    unsafe {
                        let _ = KillTimer(hwnd, crate::updater_ui::T_UPDATE);
                    }
                }
                self.flash(if on { "Automatic update checks on" } else { "Automatic update checks off" });
                changed = false;
            }
            Cmd::About => {
                let msg = format!(
                    "Slate {}\nA small, fast text editor.\n\nSettings and session: {}\nPress Ctrl+Shift+P (or F1) for every command.",
                    env!("CARGO_PKG_VERSION"),
                    settings::dir().display()
                );
                sys::info(hwnd, &msg);
                changed = false;
            }
        }
        if changed && !self.bufs.is_empty() {
            self.edited();
        }
        if !self.bufs.is_empty() {
            self.reveal_caret();
            self.recount_if_open();
        }
        self.update_title();
        self.redraw();
    }

    pub fn recount_if_open(&mut self) {
        if self.find.is_some() {
            self.recount();
        }
    }
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if start {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            start = false;
        } else {
            out.push(c);
            start = true;
        }
    }
    out
}

/// Palette match: substring, all words present, or a subsequence whose runs
/// start at word boundaries (at most one run may start mid-word).
pub fn fuzzy(q: &str, s: &str) -> Option<i32> {
    let q = q.trim();
    if q.is_empty() {
        return Some(0);
    }
    if let Some(k) = s.find(q) {
        return Some(1000 - k as i32);
    }
    if q.contains(' ') && q.split_whitespace().all(|w| s.contains(w)) {
        return Some(800);
    }
    let qc: Vec<char> = q.chars().filter(|c| *c != ' ').collect();
    let sc: Vec<char> = s.chars().collect();
    let (mut qi, mut cost, mut runs) = (0, 0, 0);
    let mut prev = false;
    for (i, c) in sc.iter().enumerate() {
        if qi < qc.len() && *c == qc[qi] {
            if !prev {
                runs += 1;
                if i > 0 && sc[i - 1].is_alphanumeric() {
                    cost += 1;
                }
            }
            prev = true;
            qi += 1;
        } else {
            prev = false;
        }
    }
    (qi == qc.len() && cost <= 1).then_some(500 - runs * 10 - cost * 50)
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' }).collect()
}

fn strip_unc(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(r) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{r}"))
    } else if let Some(r) = s.strip_prefix(r"\\?\") {
        PathBuf::from(r)
    } else {
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_ranks_substrings_first() {
        assert!(fuzzy("save", "file: save as…").unwrap() > fuzzy("sa", "file: save as…").unwrap() - 600);
        assert!(fuzzy("xyz", "file: save").is_none());
        assert!(fuzzy("sort", "file: close other tabs").is_none());
        assert!(fuzzy("fif", "search: find in files…").is_some());
        assert!(fuzzy("close tab", "file: close other tabs").is_some());
        assert_eq!(title_case("hello wORLD-x"), "Hello World-X");
        assert_eq!(strip_unc(PathBuf::from(r"\\?\D:\a")), PathBuf::from(r"D:\a"));
    }
}
