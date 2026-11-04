//! The CSV grid view: geometry, painting, hit testing, keyboard and mouse.
//! The table model lives in `csv.rs`; edits go through the buffer, so
//! undo, save and backups work as in the text view.

use crate::app::{App, Drag, Hit, Input, PalAct, PalItem, PalMode, SCROLL_W, TAB_H};
use crate::buffer::EditKind;
use crate::csv::{self, Grid};
use crate::gfx::C;
use crate::icons::Icon;
use crate::sys;
use crate::ui::r;
use std::time::Instant;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_TRAILING;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

pub const ROW_H: f32 = 24.0;
const HEAD_H: f32 = 28.0;
const FILT_H: f32 = 30.0;
const HBAR_H: f32 = 10.0;
const SORT_W: f32 = 18.0;
/// Filtering a large file waits for a pause in typing.
pub const T_FILTER: usize = 10;
const LIVE_FILTER_BYTES: usize = 8 << 20;

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
const VK_DELETE: u32 = 0x2E;
const VK_F2: u32 = 0x71;

/// What a point in the grid is over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GHit {
    None,
    Corner,
    Head(usize),
    Sort(usize),
    Edge(usize),
    Filter(usize),
    FilterCorner,
    RowHead(usize),
    Cell(usize, usize),
    VBar,
    HBar,
}

pub struct Geo {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub rn_w: f32,
    pub cells_x: f32,
    pub cells_w: f32,
    pub data_y: f32,
    pub data_h: f32,
    pub total: f32,
    pub hbar: bool,
}

impl Geo {
    pub fn vis(&self) -> usize {
        ((self.data_h / ROW_H).floor() as usize).max(1)
    }
}

/// Vertical scrollbar thumb (y, h).
fn vthumb(g: &Grid, geo: &Geo) -> Option<(f32, f32)> {
    let (n, vis) = (g.order.len(), geo.vis());
    if n <= vis {
        return None;
    }
    let h = (geo.data_h * vis as f32 / n as f32).clamp(28.0, geo.data_h);
    let y = geo.data_y + (geo.data_h - h) * (g.top as f32 / (n - vis) as f32).min(1.0);
    Some((y, h))
}

/// Horizontal scrollbar thumb (x, w).
fn hthumb(g: &Grid, geo: &Geo) -> Option<(f32, f32)> {
    if !geo.hbar {
        return None;
    }
    let w = (geo.cells_w * geo.cells_w / geo.total).clamp(28.0, geo.cells_w);
    let x = geo.cells_x + (geo.cells_w - w) * (g.x / (geo.total - geo.cells_w)).min(1.0);
    Some((x, w))
}

/// Cell text on one line.
fn one_line(v: &str) -> std::borrow::Cow<'_, str> {
    if v.contains('\n') {
        v.replace("\r\n", "\n").replace('\n', " \u{21B5} ").into()
    } else {
        v.into()
    }
}

impl App {
    pub fn grid_active(&self) -> bool {
        if self.views[self.av].tabs.is_empty() || self.bufs.is_empty() {
            return false;
        }
        let b = self.cur_ref();
        b.grid.is_some() && b.loading.is_none()
    }

    pub fn grid_of(&self, v: usize) -> &Grid {
        self.buf_of(v).grid.as_deref().unwrap()
    }

    fn grid_of_mut(&mut self, v: usize) -> &mut Grid {
        self.buf_of_mut(v).grid.as_deref_mut().unwrap()
    }

    /// Re-indexes after the text changed under the grid (undo, reload, or
    /// an edit in the other view).
    pub fn sync_grid(&mut self, v: usize) {
        let b = self.buf_of_mut(v);
        let gen = b.gen;
        if let Some(g) = b.grid.as_mut() {
            if g.gen != gen {
                g.edit = None;
                g.reindex(&mut b.doc, gen);
            }
        }
    }

    fn grid_refilter(&mut self, v: usize) {
        let b = self.buf_of_mut(v);
        if let Some(g) = b.grid.as_mut() {
            g.refilter(&mut b.doc);
        }
    }

    pub fn grid_geo(&self, v: usize) -> Geo {
        let (vx, vy, vw, vh) = self.view_rect(v);
        let g = self.grid_of(v);
        let digits = (g.records().max(1) as f32).log10().floor() + 1.0;
        let rn_w = (18.0 + digits.max(2.0) * 7.5).round();
        let (x, y, w, h) = (vx, vy + TAB_H, (vw - SCROLL_W).max(10.0), (vh - TAB_H).max(0.0));
        let total: f32 = g.widths.iter().sum();
        let cells_x = x + rn_w;
        let cells_w = (w - rn_w).max(10.0);
        let hbar = total > cells_w;
        let data_y = y + HEAD_H + FILT_H;
        let data_h = (y + h - data_y - if hbar { HBAR_H } else { 0.0 }).max(0.0);
        Geo { x, y, w, h, rn_w, cells_x, cells_w, data_y, data_h, total, hbar }
    }

    /// Columns on screen: (index, left x, width).
    fn grid_cols(g: &Grid, geo: &Geo) -> Vec<(usize, f32, f32)> {
        let mut out = Vec::new();
        let mut x = geo.cells_x - g.x;
        for (k, &w) in g.widths.iter().enumerate() {
            if x >= geo.cells_x + geo.cells_w {
                break;
            }
            if x + w > geo.cells_x {
                out.push((k, x, w));
            }
            x += w;
        }
        out
    }

    pub fn grid_hit(&self, v: usize, x: f32, y: f32) -> GHit {
        let geo = self.grid_geo(v);
        let g = self.grid_of(v);
        if x >= geo.x + geo.w {
            return GHit::VBar;
        }
        if geo.hbar && y >= geo.y + geo.h - HBAR_H {
            return GHit::HBar;
        }
        let cols = Self::grid_cols(g, &geo);
        let col = cols.iter().find(|c| x >= c.1 && x < c.1 + c.2).copied();
        if y < geo.y + HEAD_H {
            if x < geo.cells_x {
                return GHit::Corner;
            }
            if let Some(&(k, ..)) = cols.iter().find(|c| (x - (c.1 + c.2)).abs() <= 4.0) {
                return GHit::Edge(k);
            }
            return match col {
                Some((k, cx, w)) if x >= cx + w - SORT_W => GHit::Sort(k),
                Some((k, ..)) => GHit::Head(k),
                None => GHit::None,
            };
        }
        if y < geo.data_y {
            if x < geo.cells_x {
                return GHit::FilterCorner;
            }
            return col.map_or(GHit::None, |c| GHit::Filter(c.0));
        }
        let row = g.top as f32 + ((y - geo.data_y - self.scroll_offset(v)) / ROW_H).floor();
        if row < 0.0 || row as usize >= g.order.len() {
            return GHit::None;
        }
        let row = row as usize;
        if x < geo.cells_x {
            return GHit::RowHead(row);
        }
        col.map_or(GHit::None, |c| GHit::Cell(row, c.0))
    }

    /// Nearest cell to a point, for drag selection.
    fn grid_cell_near(&self, v: usize, x: f32, y: f32) -> (usize, usize) {
        let geo = self.grid_geo(v);
        let g = self.grid_of(v);
        let row = (g.top as f32 + ((y - geo.data_y) / ROW_H).floor()).max(0.0) as usize;
        let row = row.min(g.order.len().saturating_sub(1));
        let mut cx = geo.cells_x - g.x;
        let mut col = g.ncols - 1;
        for (k, &w) in g.widths.iter().enumerate() {
            if x < cx + w {
                col = k;
                break;
            }
            cx += w;
        }
        (row, col)
    }

    // ---- scrolling -------------------------------------------------------------------

    /// Scrolls by `delta` rows and glides there.
    pub fn grid_scroll(&mut self, v: usize, delta: isize) {
        let vis = self.grid_geo(v).vis();
        let g = self.grid_of_mut(v);
        let max_top = g.order.len().saturating_sub(vis);
        let old = g.top;
        g.top = (g.top as isize + delta).clamp(0, max_top as isize) as usize;
        let moved = g.top as isize - old as isize;
        if moved != 0 {
            let start = self.scroll_offset(v) + moved as f32 * ROW_H;
            self.scroll_anim[v] = Some(crate::anim::Tween::new(start, 0.0, 160.0));
        }
    }

    pub fn grid_scroll_x(&mut self, v: usize, dx: f32) {
        let geo = self.grid_geo(v);
        let g = self.grid_of_mut(v);
        g.x = (g.x + dx).clamp(0.0, (geo.total - geo.cells_w).max(0.0));
    }

    /// Scrolls so the current cell is on screen.
    pub fn grid_reveal(&mut self, v: usize) {
        let geo = self.grid_geo(v);
        let vis = geo.vis();
        let g = self.grid_of_mut(v);
        let r = g.cur.0;
        if r < g.top {
            g.top = r;
        } else if r >= g.top + vis {
            g.top = r + 1 - vis;
        }
        let c = g.cur.1.min(g.widths.len().saturating_sub(1));
        let left: f32 = g.widths[..c].iter().sum();
        let w = g.widths.get(c).copied().unwrap_or(0.0);
        if left < g.x {
            g.x = left;
        } else if left + w > g.x + geo.cells_w {
            g.x = (left + w - geo.cells_w).min(left);
        }
    }

    // ---- painting ----------------------------------------------------------------------

    pub fn paint_grid(&mut self, v: usize) {
        self.sync_grid(v);
        let geo = self.grid_geo(v);
        let active = v == self.av;
        let off = self.scroll_offset(v);
        let b = self.buf_of(v);
        let g = b.grid.as_deref().unwrap();
        let gx = &self.gfx;
        let ui = gx.ui.as_ref().unwrap();
        let bold = gx.ui_bold.as_ref().unwrap();
        let small = gx.ui_small.as_ref().unwrap();
        let cols = Self::grid_cols(g, &geo);
        let (r0, r1, c0, c1) = g.block();
        let hover = if self.hover == Hit::Grid(v) { self.grid_hover } else { GHit::None };
        gx.fill(geo.x, geo.y, geo.w + SCROLL_W, geo.h, C::Bg);

        // Data rows.
        let above = (off / ROW_H).ceil().max(0.0) as usize;
        let first = g.top.saturating_sub(above);
        let last = (g.top + geo.vis() + 2).min(g.order.len());
        let row_y = |r: usize| geo.data_y + (r as f32 - g.top as f32) * ROW_H + off;
        gx.clip(geo.cells_x, geo.data_y, geo.cells_w, geo.data_h);
        let mut cur_rect = None;
        for rr in first..last {
            let y = row_y(rr);
            if y + ROW_H <= geo.data_y || y >= geo.data_y + geo.data_h {
                continue;
            }
            if rr % 2 == 1 {
                gx.fill_alpha(geo.cells_x, y, geo.cells_w, ROW_H, 0.0, C::Hover, 0.5);
            }
            let vals = g.row(&b.doc, g.order[rr] as usize);
            for &(k, x, w) in &cols {
                if rr >= r0 && rr <= r1 && k >= c0 && k <= c1 {
                    gx.fill(x, y, w, ROW_H, if active { C::Sel } else { C::SelDim });
                }
                if (rr, k) == g.cur {
                    cur_rect = Some((x, y, w));
                }
                let Some(val) = vals.get(k).filter(|s| !s.is_empty()) else { continue };
                // Numbers right-aligned; codes with leading zeros (0012) stay left.
                let right = csv::num(val).is_some() && !(val.len() > 1 && val.starts_with('0') && !val.starts_with("0."));
                if right {
                    unsafe {
                        let _ = ui.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
                    }
                }
                gx.text(&one_line(val), ui, x + 7.0, y, w - 14.0, ROW_H, C::Fg);
                if right {
                    unsafe {
                        let _ = ui.SetTextAlignment(Default::default());
                    }
                }
            }
            gx.fill_alpha(geo.cells_x, y + ROW_H - 1.0, geo.cells_w, 1.0, 0.0, C::Border, 0.6);
        }
        for &(_, x, w) in &cols {
            gx.fill_alpha(x + w - 1.0, geo.data_y, 1.0, geo.data_h, 0.0, C::Border, 0.6);
        }
        if let Some((x, y, w)) = cur_rect {
            if g.edit.is_none() {
                gx.stroke_round(x - 1.0, y - 1.0, w + 1.0, ROW_H + 1.0, 2.0, if active { C::Accent } else { C::Dim }, 2.0);
            }
        }
        if g.order.is_empty() {
            let msg = if g.filtered() { "No rows match the filters" } else { "No data rows" };
            gx.text(msg, ui, geo.cells_x + 12.0, geo.data_y + 8.0, geo.cells_w - 24.0, ROW_H, C::Faint);
        }
        gx.unclip();

        // Record numbers, as in the file.
        gx.clip(geo.x, geo.data_y, geo.rn_w, geo.data_h);
        gx.fill(geo.x, geo.data_y, geo.rn_w, geo.data_h, C::Tabs);
        unsafe {
            let _ = small.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
        }
        for rr in first..last {
            let y = row_y(rr);
            let sel = rr >= r0 && rr <= r1 && !g.order.is_empty();
            if sel {
                gx.fill(geo.x, y, geo.rn_w, ROW_H, C::SelDim);
            }
            gx.text(&(g.order[rr] + 1).to_string(), small, geo.x, y, geo.rn_w - 8.0, ROW_H, if sel { C::Fg } else { C::Faint });
        }
        unsafe {
            let _ = small.SetTextAlignment(Default::default());
        }
        gx.unclip();

        // Header and filter rows.
        let hy = geo.y;
        let fy = hy + HEAD_H;
        gx.fill(geo.x, hy, geo.w, HEAD_H, C::Tabs);
        gx.fill(geo.x, fy, geo.w, FILT_H, C::Bg);
        let names = g.header_names(&b.doc);
        gx.clip(geo.cells_x, hy, geo.cells_w, HEAD_H + FILT_H);
        for &(k, x, w) in &cols {
            if k >= c0 && k <= c1 && !g.order.is_empty() {
                gx.fill(x, hy, w, HEAD_H, C::SelDim);
                if g.whole_cols {
                    gx.fill(x, hy + HEAD_H - 2.0, w, 2.0, C::Accent);
                }
            } else if matches!(hover, GHit::Head(h) | GHit::Sort(h) if h == k) {
                gx.fill(x, hy, w, HEAD_H, C::Hover);
            }
            gx.text(&names[k], bold, x + 8.0, hy, w - 10.0 - SORT_W, HEAD_H, C::Fg);
            match g.sort {
                Some((sk, asc)) if sk == k => gx.draw_icon(if asc { Icon::ChevronUp } else { Icon::ChevronDown }, x + w - SORT_W - 3.0, hy, SORT_W, HEAD_H, C::Accent, 10.0, 1.6),
                _ if matches!(hover, GHit::Head(h) | GHit::Sort(h) if h == k) => {
                    gx.draw_icon(Icon::ChevronDown, x + w - SORT_W - 3.0, hy, SORT_W, HEAD_H, if hover == GHit::Sort(k) { C::Fg } else { C::Faint }, 10.0, 1.3)
                }
                _ => {}
            }
            gx.fill(x + w - 1.0, hy + 6.0, 1.0, HEAD_H - 12.0, C::Border);
            let rc = r(x + 3.0, fy + 3.0, w - 6.0, FILT_H - 6.0);
            let f = &g.filters[k];
            if g.filter_focus == Some(k) && active {
                self.draw_input(f, &rc, true, "Filter", 0.0);
            } else if !f.text.is_empty() {
                gx.fill_round(rc.x, rc.y, rc.w, rc.h, 5.0, C::SelDim);
                gx.stroke_round(rc.x, rc.y, rc.w, rc.h, 5.0, C::Accent, 1.0);
                gx.text(&f.text, small, rc.x + 8.0, rc.y, rc.w - 12.0, rc.h, C::Fg);
            } else {
                let hot = hover == GHit::Filter(k);
                if hot {
                    gx.stroke_round(rc.x, rc.y, rc.w, rc.h, 5.0, C::Border, 1.0);
                    gx.text("Filter", small, rc.x + 21.0, rc.y, rc.w - 25.0, rc.h, C::Faint);
                }
                gx.draw_icon(Icon::Search, rc.x + 4.0, rc.y, 14.0, rc.h, C::Faint, 10.0, 1.2);
            }
        }
        gx.unclip();
        gx.fill(geo.x, hy, geo.rn_w, HEAD_H + FILT_H, C::Tabs);
        if g.filtered() {
            let hot = hover == GHit::FilterCorner;
            if hot {
                gx.fill(geo.x, fy, geo.rn_w, FILT_H, C::Hover);
            }
            gx.draw_icon(Icon::Close, geo.x, fy, geo.rn_w, FILT_H, if hot { C::Fg } else { C::Accent }, 9.0, 1.5);
        }
        gx.fill(geo.x, fy - 1.0, geo.w, 1.0, C::Border);
        gx.fill(geo.x, geo.data_y - 1.0, geo.w, 1.0, C::Border);
        gx.fill(geo.cells_x - 1.0, geo.y, 1.0, geo.h, C::Border);

        // The cell editor floats over its cell.
        if let Some((er, ec, inp)) = &g.edit {
            if let Some(&(_, x, w)) = cols.iter().find(|c| c.0 == *ec) {
                let y = row_y(*er);
                if y >= geo.data_y - 1.0 && y < geo.data_y + geo.data_h {
                    self.draw_input(inp, &r(x - 2.0, y - 3.0, w.max(140.0) + 4.0, ROW_H + 6.0), active, "", 0.0);
                }
            }
        }

        // Scrollbars.
        let dragging = |d: &Drag| matches!(d, Drag::GridV { view, .. } | Drag::GridH { view, .. } if *view == v);
        let hot = dragging(&self.drag) || matches!(hover, GHit::VBar | GHit::HBar);
        if let Some((ty, th)) = vthumb(g, &geo) {
            gx.fill_round(geo.x + geo.w + 3.0, ty + 2.0, SCROLL_W - 6.0, th - 4.0, 3.0, if hot { C::Dim } else { C::Faint });
        }
        if let Some((tx, tw)) = hthumb(g, &geo) {
            let y = geo.y + geo.h - HBAR_H;
            gx.fill_round(tx + 2.0, y + 2.0, tw - 4.0, HBAR_H - 4.0, 3.0, if hot { C::Dim } else { C::Faint });
        }
    }

    /// Status bar text for the grid: (left summary, position segment).
    pub fn grid_status(&self) -> (String, String) {
        let b = self.cur_ref();
        let g = b.grid.as_deref().unwrap();
        let data = g.records() - g.first_data();
        let left = if g.filtered() {
            format!("{} of {} rows shown  \u{B7}  {} columns", g.order.len(), data, g.ncols)
        } else {
            format!("{} rows  \u{B7}  {} columns", data, g.ncols)
        };
        let (r0, r1, c0, c1) = g.block();
        let names = g.header_names(&b.doc);
        let mut pos = match g.order.get(g.cur.0) {
            Some(rec) => format!("Row {}, {}", rec + 1, names.get(g.cur.1).map(|s| s.as_str()).unwrap_or("")),
            None => "No rows".into(),
        };
        if (r0, c0) != (r1, c1) && !g.order.is_empty() {
            pos += &format!("  ({} \u{D7} {})", r1 - r0 + 1, c1 - c0 + 1);
        }
        (left, pos)
    }

    // ---- turning the grid on and off -------------------------------------------------

    pub fn enable_grid(&mut self, bi: usize) {
        let b = &mut self.bufs[bi];
        let tsv = b.path.as_ref().and_then(|p| p.extension()).map_or(false, |e| e.eq_ignore_ascii_case("tsv") || e.eq_ignore_ascii_case("tab"));
        let delim = if tsv { b'\t' } else { csv::sniff(&b.doc.text(0, b.doc.len().min(1 << 16))) };
        let gen = b.gen;
        b.grid = Some(Box::new(Grid::new(&mut b.doc, gen, delim)));
    }

    /// Opens .csv / .tsv files as a grid when the setting asks for it.
    pub fn auto_grid(&mut self, id: u32) {
        if !crate::settings::get().csv_grid {
            return;
        }
        let Some(bi) = self.bufs.iter().position(|b| b.id == id) else { return };
        let b = &self.bufs[bi];
        if b.grid.is_none() && b.loading.is_none() && b.path.as_deref().map_or(false, csv::is_table_path) {
            self.enable_grid(bi);
        }
    }

    pub fn toggle_grid(&mut self) {
        let v = self.av;
        let bi = self.bi(self.tab(v).buf);
        if self.bufs[bi].grid.is_some() {
            self.grid_commit();
            let b = &mut self.bufs[bi];
            let g = b.grid.take().unwrap();
            // The text caret lands on the cell that was current.
            if let Some(&rec) = g.order.get(g.cur.0) {
                let rec = rec as usize;
                let raw = g.record(&b.doc, rec);
                let at = csv::spans(&raw, g.delim).get(g.cur.1).map_or(raw.len(), |s| s.0);
                b.set_caret(g.starts[rec] as usize + at);
            }
            self.reveal_caret();
            self.flash("Text view \u{2013} Ctrl+Shift+G shows the grid again");
        } else {
            self.enable_grid(bi);
            let b = &mut self.bufs[bi];
            let caret = b.caret();
            let g = b.grid.as_mut().unwrap();
            let rec = g.starts.partition_point(|&s| s as usize <= caret).saturating_sub(1);
            if let Some(r) = g.order.iter().position(|&x| x as usize == rec) {
                g.cur = (r, 0);
                g.anchor = g.cur;
            }
            let msg = format!("Grid: {} rows \u{D7} {} columns, {} separated", g.records() - g.first_data(), g.ncols, csv::delim_name(g.delim).to_lowercase());
            self.grid_reveal(v);
            self.flash(&msg);
        }
    }

    pub fn grid_set_delim(&mut self, d: u8) {
        let b = self.cur();
        let gen = b.gen;
        if let Some(g) = b.grid.as_mut() {
            g.delim = d;
            g.filters.clear();
            g.sort = None;
            g.edit = None;
            g.reindex(&mut b.doc, gen);
            g.autosize(&b.doc);
        }
    }

    pub fn grid_delim_palette(&mut self) {
        let cur = self.cur_ref().grid.as_ref().map(|g| g.delim);
        let items = csv::DELIMS
            .iter()
            .map(|(d, n)| PalItem { label: n.to_string(), hint: if Some(*d) == cur { "current".into() } else { String::new() }, act: PalAct::Delim(*d) })
            .collect();
        self.open_palette(PalMode::List, "Column delimiter", items, "");
    }

    pub fn grid_toggle_header(&mut self) {
        let b = self.cur();
        if let Some(g) = b.grid.as_mut() {
            g.header = !g.header;
            g.refilter(&mut b.doc);
            g.autosize(&b.doc);
        }
    }

    pub fn grid_clear_filters(&mut self) {
        let v = self.av;
        let g = self.grid_of_mut(v);
        for f in g.filters.iter_mut() {
            f.set("");
        }
        g.sort = None;
        g.top = 0;
        self.grid_refilter(v);
    }

    /// Fits a column to its header and the rows around the view.
    fn grid_fit_col(&mut self, v: usize, c: usize) {
        let w = {
            let b = self.buf_of(v);
            let g = b.grid.as_deref().unwrap();
            let gx = &self.gfx;
            let names = g.header_names(&b.doc);
            let mut w = gx.measure(&names[c], gx.ui_bold.as_ref().unwrap()) + SORT_W + 22.0;
            let from = g.top.saturating_sub(50);
            for rr in from..(from + 250).min(g.order.len()) {
                if let Some(t) = g.row(&b.doc, g.order[rr] as usize).get(c).filter(|t| !t.is_empty()) {
                    w = w.max(gx.measure(&one_line(t), gx.ui.as_ref().unwrap()) + 18.0);
                }
            }
            w.clamp(40.0, 640.0)
        };
        self.grid_of_mut(v).widths[c] = w;
    }

    pub fn grid_fit_all(&mut self) {
        let v = self.av;
        for c in 0..self.grid_of(v).ncols {
            self.grid_fit_col(v, c);
        }
    }

    // ---- editing --------------------------------------------------------------------------

    /// Starts editing the current cell, with its value or with `initial`
    /// (typing over a cell replaces it, as in a spreadsheet).
    fn grid_begin_edit(&mut self, v: usize, initial: Option<String>) {
        let b = self.buf_of_mut(v);
        let Some(g) = b.grid.as_mut() else { return };
        let Some(&rec) = g.order.get(g.cur.0) else { return };
        let text = initial.unwrap_or_else(|| g.row(&b.doc, rec as usize).get(g.cur.1).cloned().unwrap_or_default());
        let mut inp = Input::default();
        inp.set(&text);
        inp.anchor = inp.caret;
        g.edit = Some((g.cur.0, g.cur.1, inp));
        g.anchor = g.cur;
        g.filter_focus = None;
    }

    /// Writes the cell editor's text back into the document.
    pub fn grid_commit(&mut self) {
        if !self.grid_active() {
            return;
        }
        let v = self.av;
        let bi = self.bi(self.tab(v).buf);
        let cell = {
            let b = &mut self.bufs[bi];
            let Some(g) = b.grid.as_mut() else { return };
            let Some((r, c, inp)) = g.edit.take() else { return };
            let Some(&rec) = g.order.get(r) else { return };
            let old = g.row(&b.doc, rec as usize).get(c).cloned().unwrap_or_default();
            if old == inp.text {
                return;
            }
            (rec as usize, c, inp.text)
        };
        self.grid_set(bi, &[cell], Vec::new());
    }

    /// Sets cells and appends `tail` (whole new records) as one undo step.
    fn grid_set(&mut self, bi: usize, cells: &[(usize, usize, String)], tail: Vec<u8>) {
        let b = &mut self.bufs[bi];
        let Some(mut g) = b.grid.take() else { return };
        let mut ch = g.cell_changes(&b.doc, cells);
        let appended = !tail.is_empty();
        if appended {
            let n = b.doc.len();
            ch.push((n, n, tail));
        }
        b.apply(ch.clone(), EditKind::Other);
        let len = b.doc.len();
        g.shift_after(&ch, b.gen, len);
        if appended || cells.iter().any(|c| c.1 >= g.ncols) {
            // New records or columns: index again on the next paint.
            g.gen = 0;
        }
        b.grid = Some(g);
        self.edited();
        self.redraw();
    }

    pub fn grid_clear(&mut self, v: usize) {
        let bi = self.bi(self.tab(v).buf);
        let cells: Vec<(usize, usize, String)> = {
            let g = self.grid_of(v);
            let (r0, r1, c0, c1) = g.block();
            g.order.get(r0..=r1.min(g.order.len().saturating_sub(1))).unwrap_or(&[]).iter().flat_map(|&rec| (c0..=c1).map(move |c| (rec as usize, c, String::new()))).collect()
        };
        self.grid_set(bi, &cells, Vec::new());
    }

    pub fn grid_copy(&mut self, headers: bool) {
        let v = self.av;
        let hwnd = self.hwnd;
        let b = self.buf_of_mut(v);
        let g = b.grid.as_deref().unwrap();
        let (r0, r1, c0, c1) = g.block();
        let cells = g.selection_cells(headers);
        if cells > csv::RICH_CELLS {
            // Very large copies go as tab-separated text only; Excel still
            // splits it into columns.
            let tsv = g.selection_tsv(&mut b.doc, headers);
            sys::set_clipboard_with(hwnd, &tsv, &[]);
        } else {
            sys::set_clipboard_table(hwnd, &g.selection_values(&b.doc, headers));
        }
        let rows = if g.order.is_empty() { 0 } else { r1.min(g.order.len() - 1) + 1 - r0 };
        self.flash(&format!("Copied {rows} \u{D7} {} cells \u{2013} they paste into Excel as separate columns", c1 - c0 + 1));
    }

    /// Pastes tab-separated text (as Excel copies it) from the current cell
    /// down and right; rows past the end are appended. One value pasted
    /// over a block fills the block.
    pub fn grid_paste(&mut self) {
        let v = self.av;
        let Some((t, _)) = sys::get_clipboard(self.hwnd) else { return };
        let rows = csv::parse_rows(&t, b'\t');
        if rows.is_empty() {
            return;
        }
        let bi = self.bi(self.tab(v).buf);
        let (cells, tail, end) = {
            let b = &self.bufs[bi];
            let g = b.grid.as_deref().unwrap();
            let (r0, r1, c0, c1) = g.block();
            let mut cells = Vec::new();
            let mut tail = Vec::new();
            if rows.len() == 1 && rows[0].len() == 1 && (r0 != r1 || c0 != c1) {
                for rr in r0..=r1 {
                    if let Some(&rec) = g.order.get(rr) {
                        for c in c0..=c1 {
                            cells.push((rec as usize, c, rows[0][0].clone()));
                        }
                    }
                }
                (cells, tail, (r1, c1))
            } else {
                if b.doc.len() > 0 && b.doc.byte(b.doc.len() - 1) != b'\n' {
                    tail.push(b'\n');
                }
                for (i, row) in rows.iter().enumerate() {
                    match g.order.get(r0 + i) {
                        Some(&rec) => cells.extend(row.iter().enumerate().map(|(j, val)| (rec as usize, c0 + j, val.clone()))),
                        None => {
                            for k in 0..c0 + row.len() {
                                if k > 0 {
                                    tail.push(g.delim);
                                }
                                if k >= c0 {
                                    tail.extend_from_slice(&csv::quote(&row[k - c0], g.delim));
                                }
                            }
                            tail.push(b'\n');
                        }
                    }
                }
                if tail == b"\n" {
                    tail.clear();
                }
                let w = rows.iter().map(|r| r.len()).max().unwrap_or(1);
                (cells, tail, (r0 + rows.len() - 1, c0 + w - 1))
            }
        };
        self.grid_set(bi, &cells, tail);
        self.sync_grid(v);
        let g = self.grid_of_mut(v);
        let last = g.order.len().saturating_sub(1);
        g.anchor = (g.block().0.min(last), g.block().2);
        g.cur = (end.0.min(last), end.1.min(g.ncols - 1));
        self.flash(&format!("Pasted {} row{}", rows.len(), if rows.len() == 1 { "" } else { "s" }));
    }

    pub fn grid_select_all(&mut self, v: usize) {
        let g = self.grid_of_mut(v);
        g.anchor = (0, 0);
        g.cur = (g.order.len().saturating_sub(1), g.ncols - 1);
        g.whole_cols = true;
    }

    /// Ctrl+F in the grid: type into the current column's filter.
    pub fn grid_focus_filter(&mut self) {
        let v = self.av;
        let g = self.grid_of_mut(v);
        let c = g.cur.1;
        g.filter_focus = Some(c);
        let f = &mut g.filters[c];
        f.anchor = 0;
        f.caret = f.text.len();
        self.grid_reveal(v);
    }

    fn grid_filter_changed(&mut self, v: usize) {
        {
            let g = self.grid_of_mut(v);
            g.top = 0;
            g.cur.0 = 0;
            g.anchor = g.cur;
        }
        if self.buf_of(v).doc.len() > LIVE_FILTER_BYTES {
            unsafe {
                SetTimer(self.hwnd, T_FILTER, 300, None);
            }
        } else {
            self.grid_refilter(v);
        }
    }

    pub fn on_filter_timer(&mut self) {
        unsafe {
            let _ = KillTimer(self.hwnd, T_FILTER);
        }
        for v in 0..if self.split { 2 } else { 1 } {
            if !self.views[v].tabs.is_empty() && self.buf_of(v).grid.is_some() {
                self.grid_refilter(v);
            }
        }
        self.redraw();
    }

    // ---- keyboard ------------------------------------------------------------------------

    /// Moves the current cell by (dr, dc), clamped to the table.
    fn grid_move(&mut self, v: usize, dr: isize, dc: isize) {
        let g = self.grid_of_mut(v);
        let n = g.order.len().saturating_sub(1) as isize;
        g.cur = ((g.cur.0 as isize + dr).clamp(0, n) as usize, (g.cur.1 as isize + dc).clamp(0, g.ncols as isize - 1) as usize);
        g.anchor = g.cur;
        g.whole_cols = false;
        self.grid_reveal(v);
    }

    /// Returns true when the key was consumed.
    pub fn grid_key(&mut self, vk: u32, ctrl: bool, shift: bool, alt: bool) -> bool {
        let v = self.av;
        let hwnd = self.hwnd;
        self.sync_grid(v);
        let vis = self.grid_geo(v).vis();
        // Clipboard and caret keys an input box handles itself; other
        // Ctrl shortcuts (save, close, switch tab) still reach the app.
        let input_key = !ctrl || matches!(vk, 0x41 | 0x43 | 0x56 | 0x58 | VK_LEFT | VK_RIGHT | VK_BACK | VK_DELETE | VK_HOME | VK_END);

        if self.grid_of(v).edit.is_some() {
            match vk {
                VK_ESCAPE => self.grid_of_mut(v).edit = None,
                VK_RETURN if !alt && !ctrl => {
                    self.grid_commit();
                    self.grid_move(v, if shift { -1 } else { 1 }, 0);
                }
                VK_TAB if !ctrl && !alt => {
                    self.grid_commit();
                    self.grid_move(v, 0, if shift { -1 } else { 1 });
                }
                VK_UP | VK_DOWN if !shift && !ctrl && !alt => {
                    self.grid_commit();
                    self.grid_move(v, if vk == VK_UP { -1 } else { 1 }, 0);
                }
                _ if !input_key => {
                    self.grid_commit();
                    return false;
                }
                _ => {
                    if let Some((_, _, inp)) = self.grid_of_mut(v).edit.as_mut() {
                        inp.key(vk, ctrl, shift, hwnd);
                    }
                }
            }
            self.redraw();
            return true;
        }

        if let Some(k) = self.grid_of(v).filter_focus {
            match vk {
                VK_ESCAPE | VK_RETURN | VK_DOWN => self.grid_of_mut(v).filter_focus = None,
                VK_TAB if !ctrl => {
                    let g = self.grid_of_mut(v);
                    let n = g.ncols;
                    let k = if shift { (k + n - 1) % n } else { (k + 1) % n };
                    g.filter_focus = Some(k);
                    g.cur.1 = k;
                    g.anchor = g.cur;
                    let f = &mut g.filters[k];
                    f.anchor = 0;
                    f.caret = f.text.len();
                    self.grid_reveal(v);
                }
                _ if !input_key => return false,
                _ => {
                    if self.grid_of_mut(v).filters[k].key(vk, ctrl, shift, hwnd) {
                        self.grid_filter_changed(v);
                    }
                }
            }
            self.redraw();
            return true;
        }

        let (n, ncols, (r, c)) = {
            let g = self.grid_of(v);
            (g.order.len(), g.ncols, g.cur)
        };
        let last = n.saturating_sub(1);
        let page = vis.saturating_sub(1).max(1);
        let (mut nr, mut nc, mut extend) = (r, c, shift);
        match vk {
            VK_LEFT if !alt => nc = if ctrl { 0 } else { c.saturating_sub(1) },
            VK_RIGHT if !alt => nc = if ctrl { ncols - 1 } else { (c + 1).min(ncols - 1) },
            VK_UP if !alt => nr = if ctrl { 0 } else { r.saturating_sub(1) },
            VK_DOWN if !alt => nr = if ctrl { last } else { (r + 1).min(last) },
            VK_PRIOR if !ctrl && !alt => {
                nr = r.saturating_sub(page);
                self.grid_scroll(v, -(page as isize));
            }
            VK_NEXT if !ctrl && !alt => {
                nr = (r + page).min(last);
                self.grid_scroll(v, page as isize);
            }
            VK_HOME if !alt => {
                nc = 0;
                if ctrl {
                    nr = 0;
                }
            }
            VK_END if !alt => {
                nc = ncols - 1;
                if ctrl {
                    nr = last;
                }
            }
            VK_TAB if !ctrl && !alt => {
                extend = false;
                if shift {
                    if c > 0 {
                        nc = c - 1;
                    } else if r > 0 {
                        nr = r - 1;
                        nc = ncols - 1;
                    }
                } else if c + 1 < ncols {
                    nc = c + 1;
                } else if r < last {
                    nr = r + 1;
                    nc = 0;
                }
            }
            VK_RETURN if !ctrl && !alt => {
                extend = false;
                nr = if shift { r.saturating_sub(1) } else { (r + 1).min(last) };
            }
            VK_F2 if !ctrl && !shift && !alt => {
                self.grid_begin_edit(v, None);
                self.redraw();
                return true;
            }
            VK_BACK if !ctrl && !alt => {
                self.grid_begin_edit(v, Some(String::new()));
                self.redraw();
                return true;
            }
            VK_DELETE if !ctrl && !shift && !alt => {
                self.grid_clear(v);
                return true;
            }
            VK_ESCAPE => {
                let g = self.grid_of_mut(v);
                g.anchor = g.cur;
                g.whole_cols = false;
                self.redraw();
                return true;
            }
            _ => return false,
        }
        let g = self.grid_of_mut(v);
        g.cur = (nr, nc);
        if !extend {
            g.anchor = g.cur;
            g.whole_cols = false;
        }
        self.grid_reveal(v);
        self.redraw();
        true
    }

    pub fn grid_char(&mut self, ch: char) {
        let v = self.av;
        self.sync_grid(v);
        let s = ch.to_string();
        let g = self.grid_of_mut(v);
        if let Some((_, _, inp)) = g.edit.as_mut() {
            inp.insert(&s);
        } else if let Some(k) = g.filter_focus {
            g.filters[k].insert(&s);
            self.grid_filter_changed(v);
        } else {
            self.grid_begin_edit(v, Some(s));
        }
        self.redraw();
    }

    // ---- mouse ------------------------------------------------------------------------------

    pub fn grid_mouse_down(&mut self, v: usize, x: f32, y: f32, shift: bool) {
        let now = Instant::now();
        let (t, lx, ly, n) = self.last_click;
        let clicks = if now.duration_since(t).as_millis() < 450 && (lx - x).abs() < 4.0 && (ly - y).abs() < 4.0 { n % 3 + 1 } else { 1 };
        self.last_click = (now, x, y, clicks);
        self.sync_grid(v);
        let gh = self.grid_hit(v, x, y);
        // A click inside the cell being edited keeps editing.
        if let (GHit::Cell(rr, c), Some((er, ec, _))) = (gh, &self.grid_of(v).edit) {
            if (rr, c) == (*er, *ec) && v == self.av {
                return;
            }
        }
        self.grid_commit();
        self.av = v;
        self.in_find = false;
        self.sync_grid(v);
        let geo = self.grid_geo(v);
        if !matches!(gh, GHit::Filter(_)) {
            self.grid_of_mut(v).filter_focus = None;
        }
        let mut capture = true;
        match gh {
            GHit::Cell(rr, c) => {
                let g = self.grid_of_mut(v);
                g.cur = (rr, c);
                if !shift {
                    g.anchor = (rr, c);
                }
                g.whole_cols = false;
                if clicks == 2 {
                    self.grid_begin_edit(v, None);
                    capture = false;
                } else {
                    self.drag = Drag::Grid { view: v, mode: 0 };
                }
            }
            GHit::RowHead(rr) => {
                let g = self.grid_of_mut(v);
                g.anchor = if shift { (g.anchor.0, 0) } else { (rr, 0) };
                g.cur = (rr, g.ncols - 1);
                g.whole_cols = false;
                self.drag = Drag::Grid { view: v, mode: 1 };
            }
            GHit::Head(c) => {
                let g = self.grid_of_mut(v);
                if !g.order.is_empty() {
                    g.anchor = (0, if shift { g.anchor.1 } else { c });
                    g.cur = (g.order.len() - 1, c);
                    g.whole_cols = true;
                    self.drag = Drag::Grid { view: v, mode: 2 };
                }
            }
            GHit::Sort(c) => {
                let g = self.grid_of_mut(v);
                g.sort = match g.sort {
                    Some((k, true)) if k == c => Some((c, false)),
                    Some((k, false)) if k == c => None,
                    _ => Some((c, true)),
                };
                self.grid_refilter(v);
                capture = false;
            }
            GHit::Edge(c) => {
                if clicks == 2 {
                    self.grid_fit_col(v, c);
                    capture = false;
                } else {
                    let w0 = self.grid_of(v).widths[c];
                    self.drag = Drag::GridCol { view: v, col: c, x0: x, w0 };
                }
            }
            GHit::Filter(c) => {
                let g = self.grid_of_mut(v);
                g.filter_focus = Some(c);
                let f = &mut g.filters[c];
                f.caret = f.text.len();
                f.anchor = f.caret;
                capture = false;
            }
            GHit::FilterCorner => {
                if self.grid_of(v).filtered() {
                    self.grid_clear_filters();
                }
                capture = false;
            }
            GHit::Corner => {
                self.grid_select_all(v);
                capture = false;
            }
            GHit::VBar => {
                let thumb = vthumb(self.grid_of(v), &geo);
                match thumb {
                    Some((ty, th)) if y >= ty && y < ty + th => self.drag = Drag::GridV { view: v, grab: y - ty },
                    Some((ty, th)) => {
                        let page = geo.vis() as isize;
                        self.grid_scroll(v, if y < ty { -page } else { page });
                        self.drag = Drag::GridV { view: v, grab: th / 2.0 };
                    }
                    None => capture = false,
                }
            }
            GHit::HBar => {
                let thumb = hthumb(self.grid_of(v), &geo);
                match thumb {
                    Some((tx, tw)) if x >= tx && x < tx + tw => self.drag = Drag::GridH { view: v, grab: x - tx },
                    Some((tx, tw)) => {
                        self.grid_scroll_x(v, if x < tx { -geo.cells_w } else { geo.cells_w });
                        self.drag = Drag::GridH { view: v, grab: tw / 2.0 };
                    }
                    None => capture = false,
                }
            }
            GHit::None => capture = false,
        }
        if capture {
            self.capture();
        }
        self.redraw();
    }

    /// Follows a grid drag; false when no grid drag is under way.
    pub fn grid_drag(&mut self, x: f32, y: f32) -> bool {
        match self.drag {
            Drag::Grid { view, mode } => {
                let geo = self.grid_geo(view);
                if y < geo.data_y {
                    self.grid_scroll(view, -1);
                } else if y > geo.data_y + geo.data_h {
                    self.grid_scroll(view, 1);
                }
                if x > geo.cells_x + geo.cells_w {
                    self.grid_scroll_x(view, 24.0);
                } else if x < geo.cells_x && mode != 1 {
                    self.grid_scroll_x(view, -24.0);
                }
                let (rr, c) = self.grid_cell_near(view, x, y.clamp(geo.data_y, geo.data_y + geo.data_h - 1.0));
                let g = self.grid_of_mut(view);
                match mode {
                    0 => g.cur = (rr, c),
                    1 => g.cur = (rr, g.ncols - 1),
                    _ => g.cur.1 = c,
                }
            }
            Drag::GridCol { view, col, x0, w0 } => {
                self.grid_of_mut(view).widths[col] = (w0 + x - x0).clamp(32.0, 1200.0);
            }
            Drag::GridV { view, grab } => {
                let geo = self.grid_geo(view);
                let Some((_, th)) = vthumb(self.grid_of(view), &geo) else { return true };
                let f = ((y - grab - geo.data_y) / (geo.data_h - th).max(1.0)).clamp(0.0, 1.0);
                let g = self.grid_of_mut(view);
                let max_top = g.order.len().saturating_sub(geo.vis());
                g.top = (f * max_top as f32).round() as usize;
            }
            Drag::GridH { view, grab } => {
                let geo = self.grid_geo(view);
                let Some((_, tw)) = hthumb(self.grid_of(view), &geo) else { return true };
                let f = ((x - grab - geo.cells_x) / (geo.cells_w - tw).max(1.0)).clamp(0.0, 1.0);
                self.grid_of_mut(view).x = f * (geo.total - geo.cells_w).max(0.0);
            }
            _ => return false,
        }
        self.redraw();
        true
    }

    /// Tracks the part of the grid under the mouse; repaints only for
    /// parts that show a hover state, so moving over cells costs nothing.
    pub fn grid_hover_at(&mut self, v: usize, x: f32, y: f32) {
        let gh = match self.grid_hit(v, x, y) {
            GHit::Cell(..) | GHit::RowHead(_) | GHit::Edge(_) | GHit::Corner => GHit::None,
            h => h,
        };
        if gh != self.grid_hover {
            self.grid_hover = gh;
            self.redraw();
        }
    }

    pub fn grid_wheel(&mut self, v: usize, delta: i32, sideways: bool) {
        if sideways {
            self.grid_scroll_x(v, delta as f32 / 120.0 * 80.0);
        } else {
            self.grid_scroll(v, -(delta as f32 / 120.0 * 3.0).round() as isize);
        }
        self.redraw();
    }

    pub fn grid_context(&mut self, v: usize, x: f32, y: f32, pt: POINT) {
        self.grid_commit();
        self.av = v;
        self.sync_grid(v);
        let gh = self.grid_hit(v, x, y);
        let on_cell = matches!(gh, GHit::Cell(..));
        let col = {
            let g = self.grid_of_mut(v);
            match gh {
                GHit::Cell(rr, c) => {
                    if !g.is_selected(rr, c) {
                        g.cur = (rr, c);
                        g.anchor = g.cur;
                        g.whole_cols = false;
                    }
                    c
                }
                GHit::Head(c) | GHit::Sort(c) | GHit::Filter(c) | GHit::Edge(c) => c,
                _ => g.cur.1,
            }
        };
        self.redraw();
        let (header, cleanable) = {
            let g = self.grid_of(v);
            (g.header, g.filtered() || g.sort.is_some())
        };
        let items = [
            (1, "Copy", true),
            (2, "Copy with Headers", true),
            (3, "Paste", true),
            (4, "Clear Cells", true),
            (0, "", true),
            (5, "Sort Ascending", true),
            (6, "Sort Descending", true),
            (7, "Filter to This Value", on_cell),
            (8, "Clear Filters and Sort", cleanable),
            (0, "", true),
            (9, if header { "Treat First Row as Data" } else { "Use First Row as Header" }, true),
            (10, "Autofit Columns", true),
            (11, "Delimiter\u{2026}", true),
            (0, "", true),
            (12, "Edit Cell", on_cell),
            (13, "Show in Text View", true),
        ];
        let choice = sys::popup_menu(self.hwnd, pt, &items);
        match choice {
            1 => self.grid_copy(false),
            2 => self.grid_copy(true),
            3 => self.grid_paste(),
            4 => self.grid_clear(v),
            5 | 6 => {
                self.grid_of_mut(v).sort = Some((col, choice == 5));
                self.grid_refilter(v);
            }
            7 => {
                let val = {
                    let b = self.buf_of(v);
                    let g = b.grid.as_deref().unwrap();
                    g.order.get(g.cur.0).and_then(|&rec| g.row(&b.doc, rec as usize).get(col).cloned()).unwrap_or_default()
                };
                self.grid_of_mut(v).filters[col].set(&format!("={val}"));
                self.grid_filter_changed(v);
            }
            8 => self.grid_clear_filters(),
            9 => self.grid_toggle_header(),
            10 => self.grid_fit_all(),
            11 => self.grid_delim_palette(),
            12 => self.grid_begin_edit(v, None),
            13 => self.toggle_grid(),
            _ => {}
        }
        self.redraw();
    }
}
