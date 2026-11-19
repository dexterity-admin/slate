//! Slate's own dialogs and pop-up menus, drawn like the rest of the window
//! (theme colours, accent, Segoe UI) instead of Windows' grey message boxes
//! and menus. Each runs modally like the Windows ones it replaces: the call
//! returns the answer, while the main window keeps painting and receiving
//! timers and network events.

use crate::gfx::{Gfx, C};
use crate::settings;
use std::cell::Cell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_POINT_2F;
use windows::Win32::Graphics::Direct2D::{D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_ELLIPSE};
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetKeyState, IsWindowEnabled, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_CONTROL, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::*;

/// The picture beside a dialog's heading.
#[derive(Clone, Copy, PartialEq)]
pub enum Mark {
    Logo,
    Warn,
    Danger,
}

/// A dialog: heading, optional sub-line, text, label/value rows and buttons.
pub struct Spec {
    pub mark: Mark,
    pub title: String,
    pub sub: String,
    /// Wrapped text; lines starting with "- " become bullets. Scrolls when long.
    pub body: String,
    /// (label, value, what clicking the value opens: a URL or a folder).
    pub rows: Vec<(String, String, Option<String>)>,
    pub buttons: Vec<String>,
    /// The filled button, chosen by Enter.
    pub primary: usize,
    /// The answer for Esc and the close button.
    pub cancel: usize,
    pub width: f32,
}

impl Spec {
    pub fn new(mark: Mark, title: &str, body: &str) -> Spec {
        Spec { mark, title: title.into(), sub: String::new(), body: body.into(), rows: Vec::new(), buttons: vec!["OK".into()], primary: 0, cancel: 0, width: 440.0 }
    }
    pub fn buttons(mut self, b: &[&str], primary: usize, cancel: usize) -> Spec {
        self.buttons = b.iter().map(|s| s.to_string()).collect();
        self.primary = primary;
        self.cancel = cancel;
        self
    }
}

const HEAD_H: f32 = 34.0;
const PAD: f32 = 24.0;
const MARK: f32 = 40.0;
const BAR_H: f32 = 60.0;
const BTN_H: f32 = 32.0;
const ROW_H: f32 = 26.0;
const BODY_MAX: f32 = 300.0;
const ITEM_H: f32 = 30.0;
const SEP_H: f32 = 9.0;

struct Fonts {
    head: IDWriteTextFormat,
    body: IDWriteTextFormat,
    center: IDWriteTextFormat,
    small: IDWriteTextFormat,
    glyph: IDWriteTextFormat,
}

fn fonts(g: &Gfx) -> Option<Fonts> {
    unsafe {
        let head = g.format("Segoe UI", 17.0, DWRITE_FONT_WEIGHT_SEMI_BOLD).ok()?;
        let body = g.format("Segoe UI", 13.0, DWRITE_FONT_WEIGHT_NORMAL).ok()?;
        let center = g.format("Segoe UI", 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD).ok()?;
        let small = g.format("Segoe UI", 12.5, DWRITE_FONT_WEIGHT_NORMAL).ok()?;
        let glyph = g.format("Segoe UI", 20.0, DWRITE_FONT_WEIGHT_BOLD).ok()?;
        for f in [&center, &glyph] {
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER).ok()?;
        }
        for f in [&center, &small, &glyph] {
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER).ok()?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP).ok()?;
        }
        let sign = g.dw.CreateEllipsisTrimmingSign(&small).ok()?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        small.SetTrimming(&trim, &sign).ok()?;
        Some(Fonts { head, body, center, small, glyph })
    }
}

fn new_gfx() -> Option<Gfx> {
    let mut g = Gfx::new().ok()?;
    g.dark = settings::get().dark;
    g.accent = settings::get().accent;
    g.make_fonts("Consolas", 13.0, 4).ok()?;
    Some(g)
}

/// Body text with "- " lines shown as bullets.
fn body_text(s: &str) -> String {
    s.lines().map(|l| if let Some(r) = l.strip_prefix("- ") { format!("\u{2022}  {r}") } else { l.to_string() }).collect::<Vec<_>>().join("\n")
}

// ---- dialog ---------------------------------------------------------------------

struct Dlg {
    spec: Spec,
    body: String,
    g: Gfx,
    f: Fonts,
    scale: f32,
    w: f32,
    h: f32,
    text_x: f32,
    title_h: f32,
    sub_h: f32,
    body_h: f32,
    body_view: f32,
    scroll: f32,
    btns: Vec<(f32, f32, f32, f32)>,
    hover: Option<Part>,
    press: Option<Part>,
    focus: usize,
    /// Keyboard focus is shown only once the keyboard was used.
    keys: bool,
    tracking: bool,
    done: Option<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum Part {
    Button(usize),
    Close,
    Link(usize),
}

impl Dlg {
    fn layout(&mut self) {
        let content_w = self.w - self.text_x - PAD;
        let g = &self.g;
        self.title_h = g.wrapped_size(&self.spec.title, &self.f.head, content_w).1;
        self.sub_h = if self.spec.sub.is_empty() { 0.0 } else { g.wrapped_size(&self.spec.sub, &self.f.body, content_w).1 + 4.0 };
        self.body_h = if self.body.is_empty() { 0.0 } else { g.wrapped_size(&self.body, &self.f.body, content_w - 10.0).1 };
        self.body_view = self.body_h.min(BODY_MAX);
        let content = self.title_h + self.sub_h + if self.body_h > 0.0 { 12.0 + self.body_view } else { 0.0 } + if self.spec.rows.is_empty() { 0.0 } else { 14.0 + ROW_H * self.spec.rows.len() as f32 };
        self.h = HEAD_H + 6.0 + content.max(MARK) + PAD + BAR_H;
        // Buttons, right to left, each wide enough for its label.
        let mut x = self.w - PAD;
        let mut v = vec![(0.0, 0.0, 0.0, 0.0); self.spec.buttons.len()];
        for (i, b) in self.spec.buttons.iter().enumerate().rev() {
            let bw = (self.g.measure(b, &self.f.center) + 32.0).max(92.0);
            x -= bw;
            v[i] = (x, self.h - BAR_H + (BAR_H - BTN_H) / 2.0, bw, BTN_H);
            x -= 8.0;
        }
        self.btns = v;
    }

    fn rows_y(&self) -> f32 {
        HEAD_H + 6.0 + self.title_h + self.sub_h + if self.body_h > 0.0 { 12.0 + self.body_view } else { 0.0 } + 14.0
    }

    fn hit(&self, x: f32, y: f32) -> Option<Part> {
        if y < HEAD_H && x >= self.w - 46.0 {
            return Some(Part::Close);
        }
        for (i, &(bx, by, bw, bh)) in self.btns.iter().enumerate() {
            if x >= bx && x < bx + bw && y >= by && y < by + bh {
                return Some(Part::Button(i));
            }
        }
        let ry = self.rows_y();
        for (i, r) in self.spec.rows.iter().enumerate() {
            let y0 = ry + i as f32 * ROW_H;
            if r.2.is_some() && y >= y0 && y < y0 + ROW_H && x >= self.text_x + 120.0 && x < self.w - PAD {
                return Some(Part::Link(i));
            }
        }
        None
    }

    fn paint(&self) {
        let g = &self.g;
        let (w, h) = (self.w, self.h);
        unsafe {
            g.rt().BeginDraw();
        }
        g.fill(0.0, 0.0, w, h, C::Popup);
        // Header: app name and a close button; the window drags from here.
        if let Some(bmp) = g.logo((16.0 * self.scale) as u32, self.scale) {
            unsafe {
                g.rt().DrawBitmap(&bmp, Some(&crate::gfx::rect(14.0, (HEAD_H - 16.0) / 2.0, 16.0, 16.0)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
        }
        g.text("Slate", &self.f.small, 38.0, 0.0, 200.0, HEAD_H, C::Dim);
        let close_hot = self.hover == Some(Part::Close);
        if close_hot {
            g.fill_tint(w - 46.0, 0.0, 46.0, HEAD_H, 0xE81123, 1.0);
        }
        g.draw_icon(crate::icons::Icon::Close, w - 46.0, 0.0, 46.0, HEAD_H, if close_hot { C::OnAccent } else { C::Dim }, 12.0, 1.3);
        // Mark.
        let my = HEAD_H + 6.0;
        match self.spec.mark {
            Mark::Logo => {
                if let Some(bmp) = g.logo((MARK * self.scale) as u32, self.scale) {
                    unsafe {
                        g.rt().DrawBitmap(&bmp, Some(&crate::gfx::rect(PAD, my, MARK, MARK)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                    }
                }
            }
            m => {
                let (rgb, glyph) = match m {
                    Mark::Warn => (0xD29922, "!"),
                    _ => (0xE5534B, "!"),
                };
                let e = D2D1_ELLIPSE { point: D2D_POINT_2F { x: PAD + MARK / 2.0, y: my + MARK / 2.0 }, radiusX: MARK / 2.0, radiusY: MARK / 2.0 };
                unsafe {
                    g.rt().FillEllipse(&e, &g.tint(rgb, 0.16));
                    let v: Vec<u16> = glyph.encode_utf16().collect();
                    g.rt().DrawText(&v, &self.f.glyph, &crate::gfx::rect(PAD, my, MARK, MARK), &g.tint(rgb, 1.0), windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }
            }
        }
        // Heading, sub-line, text.
        let x = self.text_x;
        let cw = w - x - PAD;
        let mut y = my;
        g.text_wrapped(&self.spec.title, &self.f.head, x, y, cw, C::Fg);
        y += self.title_h;
        if !self.spec.sub.is_empty() {
            g.text_wrapped(&self.spec.sub, &self.f.body, x, y + 2.0, cw, C::Dim);
            y += self.sub_h;
        }
        if self.body_h > 0.0 {
            y += 12.0;
            let scrolls = self.body_h > self.body_view;
            if scrolls {
                g.fill_round(x - 8.0, y - 6.0, cw + 8.0, self.body_view + 12.0, 6.0, C::Chrome);
            }
            g.clip(x - 8.0, y, cw + 8.0, self.body_view);
            g.text_wrapped(&self.body, &self.f.body, x, y - self.scroll, cw - 10.0, C::Fg);
            g.unclip();
            if scrolls {
                let th = (self.body_view * self.body_view / self.body_h).max(24.0);
                let ty = y + (self.body_view - th) * self.scroll / (self.body_h - self.body_view);
                g.fill_round(x + cw - 6.0, ty, 4.0, th, 2.0, C::Faint);
            }
            y += self.body_view;
        }
        if !self.spec.rows.is_empty() {
            y += 14.0;
            for (i, (label, value, link)) in self.spec.rows.iter().enumerate() {
                let yy = y + i as f32 * ROW_H;
                g.text(label, &self.f.small, x, yy, 116.0, ROW_H, C::Dim);
                let hot = self.hover == Some(Part::Link(i));
                g.text(value, &self.f.small, x + 120.0, yy, cw - 120.0, ROW_H, if link.is_some() { C::Accent } else { C::Fg });
                if hot {
                    let tw = g.measure(value, &self.f.small).min(cw - 120.0);
                    g.line(x + 120.0, yy + ROW_H - 5.0, x + 120.0 + tw, yy + ROW_H - 5.0, C::Accent, 1.0);
                }
            }
        }
        // Button bar.
        g.fill(0.0, h - BAR_H, w, BAR_H, C::Chrome);
        g.line(0.0, h - BAR_H + 0.5, w, h - BAR_H + 0.5, C::Border, 1.0);
        for (i, (label, &(bx, by, bw, bh))) in self.spec.buttons.iter().zip(&self.btns).enumerate() {
            let hot = self.hover == Some(Part::Button(i));
            let down = hot && self.press == Some(Part::Button(i));
            if i == self.spec.primary {
                g.fill_round(bx, by, bw, bh, 5.0, C::Accent);
                if hot {
                    g.fill_round(bx, by, bw, bh, 5.0, C::Bg);
                    g.fill_alpha(bx, by, bw, bh, 5.0, C::Accent, if down { 0.75 } else { 0.88 });
                }
                g.text(label, &self.f.center, bx, by, bw, bh, C::OnAccent);
            } else {
                g.fill_round(bx, by, bw, bh, 5.0, if hot { C::Hover } else { C::Popup });
                if down {
                    g.fill_alpha(bx, by, bw, bh, 5.0, C::Border, 0.6);
                }
                g.stroke_round(bx, by, bw, bh, 5.0, C::Border, 1.0);
                g.text(label, &self.f.center, bx, by, bw, bh, C::Fg);
            }
            if self.keys && i == self.focus {
                g.stroke_round(bx - 3.0, by - 3.0, bw + 6.0, bh + 6.0, 7.0, C::Accent, 2.0);
            }
        }
        g.stroke_round(0.0, 0.0, w, h, 0.0, C::Border, 1.0);
        unsafe {
            let _ = g.rt().EndDraw(None, None);
        }
    }

    fn scroll_by(&mut self, d: f32) {
        let max = (self.body_h - self.body_view).max(0.0);
        self.scroll = (self.scroll + d).clamp(0.0, max);
    }

    fn copy_text(&self) -> String {
        let mut s = self.spec.title.clone();
        for t in [&self.spec.sub, &self.spec.body] {
            if !t.is_empty() {
                s += "\n\n";
                s += t;
            }
        }
        for (l, v, _) in &self.spec.rows {
            s += &format!("\n{l}: {v}");
        }
        s
    }
}

fn class(name: PCWSTR, proc_: WNDPROC, bit: u8) {
    thread_local!(static DONE: Cell<u8> = const { Cell::new(0) });
    if DONE.with(|d| d.get() & bit != 0) {
        return;
    }
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW,
            lpfnWndProc: proc_,
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: name,
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
    DONE.with(|d| d.set(d.get() | bit));
}

fn work_area(pt: POINT) -> RECT {
    unsafe {
        let m = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(m, &mut mi);
        mi.rcWork
    }
}

fn dpi_of(owner: HWND) -> f32 {
    let d = unsafe { GetDpiForWindow(owner) };
    if d == 0 { 96.0 } else { d as f32 }
}

/// Windows 11 rounds the corners; Windows 10 keeps them square.
fn round_corners(hwnd: HWND, small: bool) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE};
    let pref: u32 = if small { 3 } else { 2 };
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const u32 as *const _, 4);
    }
}

/// Shows a dialog and returns the index of the button chosen.
pub fn show(owner: HWND, spec: Spec) -> usize {
    let cancel = spec.cancel;
    let Some(g) = new_gfx() else { return cancel };
    let Some(f) = fonts(&g) else { return cancel };
    let dpi = dpi_of(owner);
    let scale = dpi / 96.0;
    let text_x = PAD + MARK + 16.0;
    let body = body_text(&spec.body);
    let focus = spec.primary;
    let w = spec.width;
    let mut d = Box::new(Dlg { spec, body, g, f, scale, w, h: 0.0, text_x, title_h: 0.0, sub_h: 0.0, body_h: 0.0, body_view: 0.0, scroll: 0.0, btns: Vec::new(), hover: None, press: None, focus, keys: false, tracking: false, done: None });
    d.layout();
    let (pw, ph) = ((d.w * scale).round() as i32, (d.h * scale).round() as i32);
    // Centred on the owner, kept on its screen.
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(owner, &mut r);
    }
    let centre = POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 };
    let wa = work_area(centre);
    let x = (centre.x - pw / 2).clamp(wa.left, (wa.right - pw).max(wa.left));
    let y = (centre.y - ph / 2).clamp(wa.top, (wa.bottom - ph).max(wa.top));
    class(w!("SlateDialog"), Some(dlg_proc), 1);
    let enabled = unsafe { IsWindowEnabled(owner).as_bool() };
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let title: Vec<u16> = crate::sys::wide(&d.spec.title);
        let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("SlateDialog"), PCWSTR(title.as_ptr()), WS_POPUP, x, y, pw, ph, owner, None, hinst, None) else { return cancel };
        round_corners(hwnd, false);
        if d.g.create_target(hwnd, pw as u32, ph as u32, dpi).is_err() {
            let _ = DestroyWindow(hwnd);
            return cancel;
        }
        let raw = Box::into_raw(d);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize);
        if enabled {
            let _ = EnableWindow(owner, false);
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
        let mut msg = MSG::default();
        while (*raw).done.is_none() {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == 0 {
                PostQuitMessage(msg.wParam.0 as i32);
                break;
            }
            if r.0 == -1 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // The owner comes back before this window goes, so it gets the focus.
        if enabled {
            let _ = EnableWindow(owner, true);
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let _ = DestroyWindow(hwnd);
        let d = Box::from_raw(raw);
        d.done.unwrap_or(cancel)
    }
}

unsafe extern "system" fn dlg_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dlg;
    if raw.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let d = &mut *raw;
    let pos = |lp: LPARAM| ((lp.0 & 0xFFFF) as i16 as f32 / d.scale, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / d.scale);
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            d.paint();
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_NCHITTEST => {
            let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
            let (x, y) = (pt.x as f32 / d.scale, pt.y as f32 / d.scale);
            if y < HEAD_H && x < d.w - 46.0 {
                return LRESULT(HTCAPTION as isize);
            }
            LRESULT(HTCLIENT as isize)
        }
        WM_SETCURSOR => {
            let link = matches!(d.hover, Some(Part::Link(_)));
            SetCursor(LoadCursorW(None, if link { IDC_HAND } else { IDC_ARROW }).unwrap_or_default());
            LRESULT(1)
        }
        WM_MOUSEMOVE => {
            let (x, y) = pos(lp);
            let h = d.hit(x, y);
            if !d.tracking {
                let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut t);
                d.tracking = true;
            }
            if h != d.hover {
                d.hover = h;
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        0x02A3 => {
            d.tracking = false;
            d.hover = None;
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = pos(lp);
            d.press = d.hit(x, y);
            SetCapture(hwnd);
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            let (x, y) = pos(lp);
            let h = d.hit(x, y);
            if h.is_some() && h == d.press {
                match h {
                    Some(Part::Button(i)) => d.done = Some(i),
                    Some(Part::Close) => d.done = Some(d.spec.cancel),
                    Some(Part::Link(i)) => {
                        if let Some(t) = d.spec.rows[i].2.clone() {
                            open_target(&t);
                        }
                    }
                    None => {}
                }
            }
            d.press = None;
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f32;
            d.scroll_by(-delta / 120.0 * 48.0);
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wp.0 as u16;
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
            let n = d.spec.buttons.len().max(1);
            match vk {
                v if v == VK_ESCAPE.0 => d.done = Some(d.spec.cancel),
                v if v == VK_RETURN.0 => d.done = Some(if d.keys { d.focus } else { d.spec.primary }),
                v if v == VK_SPACE.0 && d.keys => d.done = Some(d.focus),
                v if v == VK_TAB.0 || v == VK_RIGHT.0 || v == VK_LEFT.0 => {
                    let back = v == VK_LEFT.0 || (v == VK_TAB.0 && shift);
                    if d.keys {
                        d.focus = if back { (d.focus + n - 1) % n } else { (d.focus + 1) % n };
                    }
                    d.keys = true;
                }
                v if v == VK_UP.0 => d.scroll_by(-40.0),
                v if v == VK_DOWN.0 => d.scroll_by(40.0),
                v if v == VK_PRIOR.0 => d.scroll_by(-d.body_view),
                v if v == VK_NEXT.0 => d.scroll_by(d.body_view),
                v if v == VK_HOME.0 => d.scroll_by(-1e9),
                v if v == VK_END.0 => d.scroll_by(1e9),
                v if ctrl && v == b'C' as u16 => crate::sys::set_clipboard(hwnd, d.copy_text().as_bytes(), false),
                v if v < 0x80 && (v as u8).is_ascii_alphabetic() && !ctrl => {
                    // The first letter picks a button when only one starts with it.
                    let k = (v as u8 as char).to_ascii_lowercase();
                    let hits: Vec<usize> = d.spec.buttons.iter().enumerate().filter(|(_, b)| b.chars().next().map(|c| c.to_ascii_lowercase()) == Some(k)).map(|(i, _)| i).collect();
                    if hits.len() == 1 {
                        d.done = Some(hits[0]);
                    }
                }
                _ => return DefWindowProcW(hwnd, msg, wp, lp),
            }
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_CLOSE => {
            d.done = Some(d.spec.cancel);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Opens a URL or a folder with what Windows uses for it.
pub fn open_target(t: &str) {
    let _ = std::process::Command::new("explorer.exe").arg(t).spawn();
}

// ---- menu -----------------------------------------------------------------------

struct Menu {
    items: Vec<(u32, String, String, bool)>,
    g: Gfx,
    f: Fonts,
    scale: f32,
    w: f32,
    h: f32,
    hover: Option<usize>,
    pressed_inside: bool,
    opened: std::time::Instant,
    done: Option<u32>,
}

impl Menu {
    fn item_at(&self, x: f32, y: f32) -> Option<usize> {
        if x < 0.0 || x >= self.w || y < 0.0 {
            return None;
        }
        let mut yy = 5.0;
        for (i, it) in self.items.iter().enumerate() {
            let ih = if it.0 == 0 { SEP_H } else { ITEM_H };
            if y >= yy && y < yy + ih {
                return (it.0 != 0 && it.3).then_some(i);
            }
            yy += ih;
        }
        None
    }

    fn step(&mut self, down: bool) {
        let n = self.items.len();
        let mut i = self.hover.unwrap_or(if down { n - 1 } else { 0 });
        for _ in 0..n {
            i = if down { (i + 1) % n } else { (i + n - 1) % n };
            if self.items[i].0 != 0 && self.items[i].3 {
                self.hover = Some(i);
                return;
            }
        }
    }

    fn paint(&self) {
        let g = &self.g;
        unsafe {
            g.rt().BeginDraw();
        }
        g.fill(0.0, 0.0, self.w, self.h, C::Popup);
        let mut y = 5.0;
        for (i, (id, label, hint, en)) in self.items.iter().enumerate() {
            if *id == 0 {
                g.line(10.0, y + SEP_H / 2.0, self.w - 10.0, y + SEP_H / 2.0, C::Border, 1.0);
                y += SEP_H;
                continue;
            }
            if self.hover == Some(i) {
                g.fill_round(5.0, y + 1.0, self.w - 10.0, ITEM_H - 2.0, 4.0, C::Hover);
                g.fill_round(5.0, y + 7.0, 3.0, ITEM_H - 14.0, 1.5, C::Accent);
            }
            let c = if *en { C::Fg } else { C::Faint };
            g.text(label, &self.f.small, 18.0, y, self.w - 36.0, ITEM_H, c);
            if !hint.is_empty() {
                let hw = g.measure(hint, &self.f.small);
                g.text(hint, &self.f.small, self.w - 16.0 - hw, y, hw + 4.0, ITEM_H, if *en { C::Dim } else { C::Faint });
            }
            y += ITEM_H;
        }
        g.stroke_round(0.0, 0.0, self.w, self.h, 0.0, C::Border, 1.0);
        unsafe {
            let _ = g.rt().EndDraw(None, None);
        }
    }
}

/// Shows a pop-up menu of `(id, label, enabled)` at a screen point and
/// returns the id picked, or 0. Id 0 is a separator; a tab in a label
/// separates its shortcut, shown on the right.
pub fn menu(owner: HWND, at: POINT, items: &[(u32, &str, bool)]) -> u32 {
    let Some(g) = new_gfx() else { return 0 };
    let Some(f) = fonts(&g) else { return 0 };
    let dpi = dpi_of(owner);
    let scale = dpi / 96.0;
    let items: Vec<(u32, String, String, bool)> = items
        .iter()
        .map(|(id, l, en)| {
            let (a, b) = l.split_once('\t').unwrap_or((l, ""));
            (*id, a.to_string(), b.to_string(), *en)
        })
        .collect();
    let mut w: f32 = 180.0;
    for it in &items {
        let hw = if it.2.is_empty() { 0.0 } else { g.measure(&it.2, &f.small) + 32.0 };
        w = w.max(g.measure(&it.1, &f.small) + hw + 44.0);
    }
    let h = 10.0 + items.iter().map(|i| if i.0 == 0 { SEP_H } else { ITEM_H }).sum::<f32>();
    let (pw, ph) = ((w * scale).round() as i32, (h * scale).round() as i32);
    let wa = work_area(at);
    let x = if at.x + pw > wa.right { (at.x - pw).max(wa.left) } else { at.x };
    let y = if at.y + ph > wa.bottom { (at.y - ph).max(wa.top) } else { at.y };
    class(w!("SlateMenu"), Some(menu_proc), 2);
    let mut m = Box::new(Menu { items, g, f, scale, w, h, hover: None, pressed_inside: false, opened: std::time::Instant::now(), done: None });
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let Ok(hwnd) = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("SlateMenu"), w!(""), WS_POPUP, x, y, pw, ph, owner, None, hinst, None) else { return 0 };
        round_corners(hwnd, true);
        if m.g.create_target(hwnd, pw as u32, ph as u32, dpi).is_err() {
            let _ = DestroyWindow(hwnd);
            return 0;
        }
        let raw = Box::into_raw(m);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetCapture(hwnd);
        let mut msg = MSG::default();
        while (*raw).done.is_none() {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == 0 {
                PostQuitMessage(msg.wParam.0 as i32);
                break;
            }
            if r.0 == -1 {
                break;
            }
            // The menu takes the keyboard while it is open.
            match msg.message {
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    menu_key(&mut *raw, msg.wParam.0 as u16);
                    let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, false);
                    continue;
                }
                WM_KEYUP | WM_SYSKEYUP | WM_CHAR | WM_SYSCHAR => continue,
                _ => {}
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if (*raw).done.is_none() {
            (*raw).done = Some(0);
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let _ = ReleaseCapture();
        let _ = DestroyWindow(hwnd);
        let m = Box::from_raw(raw);
        m.done.unwrap_or(0)
    }
}

fn menu_key(m: &mut Menu, vk: u16) {
    match vk {
        v if v == VK_ESCAPE.0 || v == VK_LEFT.0 => m.done = Some(0),
        v if v == VK_DOWN.0 => m.step(true),
        v if v == VK_UP.0 => m.step(false),
        v if v == VK_RETURN.0 || v == VK_SPACE.0 || v == VK_RIGHT.0 => {
            if let Some(i) = m.hover {
                m.done = Some(m.items[i].0);
            }
        }
        v if (v as u8).is_ascii_alphanumeric() && v < 0x80 => {
            let k = (v as u8 as char).to_ascii_lowercase();
            let hits: Vec<usize> = m.items.iter().enumerate().filter(|(_, it)| it.0 != 0 && it.3 && it.1.chars().next().map(|c| c.to_ascii_lowercase()) == Some(k)).map(|(i, _)| i).collect();
            match hits.len() {
                0 => {}
                1 => m.done = Some(m.items[hits[0]].0),
                _ => {
                    let next = hits.iter().find(|&&i| Some(i) > m.hover).or(hits.first()).copied();
                    m.hover = next;
                }
            }
        }
        _ => {}
    }
}

unsafe extern "system" fn menu_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Menu;
    if raw.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let m = &mut *raw;
    let (x, y) = ((lp.0 & 0xFFFF) as i16 as f32 / m.scale, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / m.scale);
    let inside = x >= 0.0 && y >= 0.0 && x < m.w && y < m.h;
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            m.paint();
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let h = m.item_at(x, y);
            if h != m.hover && (h.is_some() || !inside) {
                m.hover = h;
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            if inside {
                m.pressed_inside = true;
            } else {
                m.done = Some(0);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            // The release of the click that opened the menu does not pick.
            let fresh = m.opened.elapsed().as_millis() < 250 && !m.pressed_inside;
            if inside && !fresh {
                if let Some(i) = m.item_at(x, y) {
                    m.done = Some(m.items[i].0);
                }
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED | WM_CANCELMODE => {
            if m.done.is_none() {
                m.done = Some(0);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Where a menu under a toolbar or title-bar button opens: below its
/// bottom-left corner, in screen pixels.
pub fn below(owner: HWND, x: f32, y: f32, scale: f32) -> POINT {
    let mut pt = POINT { x: (x * scale) as i32, y: (y * scale) as i32 };
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::ClientToScreen(owner, &mut pt);
    }
    pt
}
