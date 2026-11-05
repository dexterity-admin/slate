#![windows_subsystem = "windows"]

mod ai;
mod anim;
mod app;
mod buffer;
mod collab;
mod compare;
mod csv;
mod diff;
mod doc;
mod grid;
mod edit;
mod enc;
mod follow;
mod gfx;
mod icons;
mod index;
mod library;
mod net;
mod input;
mod paint;
mod search;
mod settings;
mod ot;
mod syntax;
mod sys;
mod team;
mod wire;
mod ui;
mod update;
mod updater_ui;
mod view;

use app::{App, Hit, T_BACKUP, T_BENCH, T_STATUS, TITLE_H, WM_FIF_DONE};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, ScreenToClient, PAINTSTRUCT};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_CONTROL, VK_F4, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Timing trace for performance work: with SLATE_TRACE set to a file
/// path, appends "<ms since start> <what>" lines to it.
pub fn trace(what: &str) {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    if let Some(p) = PATH.get_or_init(|| std::env::var("SLATE_TRACE").ok()) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(p) {
            let _ = writeln!(f, "{:>8} {what}", start.elapsed().as_millis());
        }
    }
}

fn app_from(hwnd: HWND) -> Option<&'static mut App> {
    unsafe {
        let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
        if raw.is_null() {
            None
        } else {
            Some(&mut *raw)
        }
    }
}

fn lo(l: isize) -> i32 {
    (l & 0xFFFF) as i16 as i32
}

fn hi(l: isize) -> i32 {
    ((l >> 16) & 0xFFFF) as i16 as i32
}

fn mods() -> (bool, bool, bool) {
    unsafe {
        (
            GetKeyState(VK_CONTROL.0 as i32) < 0,
            GetKeyState(VK_SHIFT.0 as i32) < 0,
            GetKeyState(VK_MENU.0 as i32) < 0,
        )
    }
}

static mut BENCH: bool = false;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_TEST_KEY: u32 = WM_APP + 2;

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCALCSIZE if wp.0 != 0 => {
            // Drop the system caption; keep the resize frame when maximized.
            if IsZoomed(hwnd).as_bool() {
                let p = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
                let dpi = GetDpiForWindow(hwnd);
                let f = GetSystemMetricsForDpi(SM_CXFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                let r = &mut p.rgrc[0];
                r.left += f;
                r.right -= f;
                r.top += f;
                r.bottom -= f;
            }
            return LRESULT(0);
        }
        WM_NCHITTEST => {
            let Some(a) = app_from(hwnd) else { return DefWindowProcW(hwnd, msg, wp, lp) };
            let mut pt = POINT { x: lo(lp.0), y: hi(lp.0) };
            let _ = ScreenToClient(hwnd, &mut pt);
            let (x, y) = (pt.x as f32 / a.scale, pt.y as f32 / a.scale);
            if !a.maximized {
                let b = 6.0;
                let (l, r, t, bt) = (x < b, x >= a.w - b, y < b, y >= a.h - b);
                let code = match (l, r, t, bt) {
                    (true, _, true, _) => HTTOPLEFT,
                    (_, true, true, _) => HTTOPRIGHT,
                    (true, _, _, true) => HTBOTTOMLEFT,
                    (_, true, _, true) => HTBOTTOMRIGHT,
                    (true, ..) => HTLEFT,
                    (_, true, ..) => HTRIGHT,
                    (_, _, true, _) => HTTOP,
                    (.., true) => HTBOTTOM,
                    _ => 0,
                };
                if code != 0 {
                    return LRESULT(code as isize);
                }
            }
            if y < TITLE_H && a.hit_test(x, y) == Hit::Caption && a.palette.is_none() {
                return LRESULT(HTCAPTION as isize);
            }
            return LRESULT(HTCLIENT as isize);
        }
        WM_NCACTIVATE => return DefWindowProcW(hwnd, msg, wp, LPARAM(-1)),
        WM_ERASEBKGND => return LRESULT(1),
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lp.0 as *mut MINMAXINFO);
            mmi.ptMinTrackSize = POINT { x: 520, y: 340 };
            // Maximise to the work area so the taskbar stays visible.
            use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
            let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                // WM_NCCALCSIZE trims this frame back off when maximised.
                let dpi = GetDpiForWindow(hwnd);
                let f = GetSystemMetricsForDpi(SM_CXFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                let (w, m) = (mi.rcWork, mi.rcMonitor);
                mmi.ptMaxPosition = POINT { x: w.left - m.left - f, y: w.top - m.top - f };
                mmi.ptMaxSize = POINT { x: w.right - w.left + 2 * f, y: w.bottom - w.top + 2 * f };
            }
            return LRESULT(0);
        }
        _ => {}
    }
    let Some(a) = app_from(hwnd) else { return DefWindowProcW(hwnd, msg, wp, lp) };
    let px = |l: LPARAM| (lo(l.0) as f32 / a.scale, hi(l.0) as f32 / a.scale);
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            if a.gfx.rt.is_none() {
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let dpi = GetDpiForWindow(hwnd) as f32;
                let _ = a.gfx.create_target(hwnd, rc.right as u32, rc.bottom as u32, dpi);
                a.cache.clear();
            }
            a.paint();
            let _ = EndPaint(hwnd, &ps);
        }
        WM_SIZE => {
            let (pw, ph) = (lo(lp.0) as u32, hi(lp.0) as u32);
            a.maximized = wp.0 as u32 == SIZE_MAXIMIZED;
            a.w = pw as f32 / a.scale;
            a.h = ph as f32 / a.scale;
            if let Some(rt) = &a.gfx.hrt {
                let _ = rt.Resize(&D2D_SIZE_U { width: pw.max(1), height: ph.max(1) });
            }
            a.redraw();
        }
        WM_DPICHANGED => {
            let dpi = (wp.0 & 0xFFFF) as f32;
            a.scale = dpi / 96.0;
            if let Some(rt) = &a.gfx.rt {
                rt.SetDpi(dpi, dpi);
            }
            let r = &*(lp.0 as *const RECT);
            let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        WM_SETFOCUS => {
            a.focused = true;
            a.redraw();
        }
        WM_KILLFOCUS => {
            // The system caret belongs to the focused window only.
            a.focused = false;
            let _ = DestroyCaret();
            a.sys_caret = None;
            a.caret_shown = false;
            a.redraw();
        }
        WM_ACTIVATEAPP => {
            if wp.0 != 0 && !BENCH {
                a.check_disk();
            }
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wp.0 as u32;
            if msg == WM_SYSKEYDOWN && vk == VK_F4.0 as u32 {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
            let (c, s, alt) = mods();
            if !a.on_key(vk, c, s, alt) && msg == WM_SYSKEYDOWN {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
        }
        WM_CHAR => {
            if let Some(ch) = char::from_u32(wp.0 as u32) {
                a.on_char(ch);
            }
        }
        WM_SYSCHAR => {
            if wp.0 == ' ' as usize {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
        }
        WM_LBUTTONDOWN => {
            let (x, y) = px(lp);
            let (c, s, alt) = mods();
            a.on_mouse_down(x, y, c, s, alt);
        }
        WM_LBUTTONUP => {
            let (x, y) = px(lp);
            a.on_mouse_up(x, y);
        }
        WM_MOUSEMOVE => {
            let (x, y) = px(lp);
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            a.on_mouse_move(x, y);
        }
        WM_MOUSELEAVE => {
            if a.hover != Hit::None {
                a.hover = Hit::None;
                a.redraw();
            }
        }
        WM_MBUTTONUP => {
            let (x, y) = px(lp);
            a.on_middle(x, y);
        }
        WM_RBUTTONUP => {
            let (x, y) = px(lp);
            a.on_context(x, y);
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let mut pt = POINT { x: lo(lp.0), y: hi(lp.0) };
            let _ = ScreenToClient(hwnd, &mut pt);
            let delta = hi(wp.0 as isize);
            let (c, s, _) = mods();
            a.on_wheel(pt.x as f32 / a.scale, pt.y as f32 / a.scale, delta, c, s, msg == WM_MOUSEHWHEEL);
        }
        WM_SETCURSOR => {
            if (lp.0 & 0xFFFF) as u32 == HTCLIENT {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(hwnd, &mut pt);
                let hit = a.hit_test(pt.x as f32 / a.scale, pt.y as f32 / a.scale);
                let id = match hit {
                    Hit::Text(_) | Hit::FindField(_) | Hit::PalInput => IDC_IBEAM,
                    Hit::Splitter => IDC_SIZEWE,
                    Hit::Grid(v) => match a.grid_hit(v, pt.x as f32 / a.scale, pt.y as f32 / a.scale) {
                        grid::GHit::Edge(_) => IDC_SIZEWE,
                        grid::GHit::Filter(_) => IDC_IBEAM,
                        _ => IDC_ARROW,
                    },
                    _ => IDC_ARROW,
                };
                if let Ok(c) = LoadCursorW(None, id) {
                    SetCursor(c);
                }
                return LRESULT(1);
            }
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        WM_TIMER => match wp.0 {
            T_BENCH => {
                let _ = DestroyWindow(hwnd);
            }
            T_BACKUP => a.write_backups(),
            app::T_LOAD | app::T_ANIM => a.redraw(),
            grid::T_FILTER => a.on_filter_timer(),
            collab::T_COLLAB => a.collab_tick(),
            updater_ui::T_UPDATE => a.on_update_timer(),
            updater_ui::T_HANDSHAKE => a.on_handshake_timer(),
            updater_ui::T_CLEANUP => a.on_cleanup_timer(),
            T_STATUS => {
                let _ = KillTimer(hwnd, T_STATUS);
                a.status = None;
                a.redraw();
            }
            _ => {}
        },
        WM_DROPFILES => {
            let hd = HDROP(wp.0 as *mut _);
            let n = DragQueryFileW(hd, u32::MAX, None);
            let mut files = Vec::new();
            for i in 0..n {
                let mut buf = vec![0u16; 4096];
                let k = DragQueryFileW(hd, i, Some(&mut buf));
                files.push(String::from_utf16_lossy(&buf[..k as usize]));
            }
            DragFinish(hd);
            a.drop_files(files);
        }
        WM_COPYDATA => {
            let cds = &*(lp.0 as *const COPYDATASTRUCT);
            if cds.dwData == 0x534C && !cds.lpData.is_null() {
                let s = std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2);
                let text = String::from_utf16_lossy(s);
                for f in text.split('\n').filter(|f| !f.is_empty()) {
                    a.open_path(f);
                }
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
                // Files handed to this window: greet them with the splash.
                a.splash_since = Some(std::time::Instant::now());
                a.redraw();
            }
            return LRESULT(1);
        }
        // Test hook: a key with explicit modifiers (bit 0 ctrl, 1 shift, 2 alt),
        // so UI scripts can drive shortcuts without real keyboard state.
        updater_ui::WM_UPDATE => {
            let m = Box::from_raw(lp.0 as *mut update::UpdateMsg);
            a.on_update_msg(*m, wp.0 != 0);
        }
        updater_ui::WM_UPDATE_OK => a.on_update_ok(),
        app::WM_LOADED => {
            let l = Box::from_raw(lp.0 as *mut app::Loaded);
            a.on_loaded(*l);
        }
        ai::WM_AI => {
            let r = Box::from_raw(lp.0 as *mut ai::Reply);
            a.on_ai(*r);
        }
        follow::WM_FOLLOW => {
            let g = Box::from_raw(lp.0 as *mut follow::Grown);
            a.on_follow(*g);
        }
        app::WM_SAVED => {
            let s = Box::from_raw(lp.0 as *mut app::Saved);
            a.on_saved(*s);
        }
        net::WM_NET => {
            let e = Box::from_raw(lp.0 as *mut net::Event);
            a.on_net(*e);
        }
        WM_TEST_KEY => {
            a.on_key(wp.0 as u32, lp.0 & 1 != 0, lp.0 & 2 != 0, lp.0 & 4 != 0);
        }
        WM_FIF_DONE => {
            let s = Box::from_raw(lp.0 as *mut String);
            a.fif_done(*s);
        }
        // Acrylic lags while a window is dragged on Windows 10: plain blur then.
        WM_ENTERSIZEMOVE | WM_EXITSIZEMOVE if a.gfx.acrylic => {
            sys::set_backdrop(hwnd, true, msg == WM_ENTERSIZEMOVE, a.gfx.dark);
        }
        WM_CLOSE => {
            // Wait for background saves; the window closes after them.
            if a.bufs.iter().any(|b| b.saving.is_some()) {
                a.close_after_save = true;
                a.flash("Saving\u{2026} Slate closes when the save is done");
                return LRESULT(0);
            }
            if !BENCH && !a.exiting_for_update {
                let restore = settings::get().restore_session;
                if restore && !a.no_session {
                    // Huge files get no backup copy, so unsaved changes in
                    // them need an answer now.
                    while let Some((v, i)) = (0..2).find_map(|v| {
                        a.views[v].tabs.iter().position(|t| {
                            let b = &a.bufs[a.bi(t.buf)];
                            b.dirty() && b.doc.len() >= app::HUGE
                        }).map(|i| (v, i))
                    }) {
                        if !a.close_tab(v, i) {
                            return LRESULT(0);
                        }
                    }
                    a.save_session();
                } else {
                    // Without session restore, unsaved work needs an answer.
                    a.split = false;
                    let moved: Vec<app::Tab> = a.views[1].tabs.drain(..).collect();
                    a.views[0].tabs.extend(moved);
                    a.av = 0;
                    while let Some(i) = a.views[0].tabs.iter().position(|t| a.bufs[a.bi(t.buf)].dirty() && Some(t.buf) != a.results_buf) {
                        if !a.close_tab(0, i) {
                            return LRESULT(0);
                        }
                    }
                    a.save_session();
                }
                let mut wpl = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                if GetWindowPlacement(hwnd, &mut wpl).is_ok() {
                    let r = wpl.rcNormalPosition;
                    settings::get_mut().win = Some((r.left, r.top, r.right - r.left, r.bottom - r.top, a.maximized));
                }
                settings::save();
            }
            let _ = DestroyWindow(hwnd);
        }
        WM_DESTROY => {
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
            if !raw.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(raw));
            }
            PostQuitMessage(0);
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

const CLASS: PCWSTR = w!("SlateEditorWindow");

/// Hands files to a running instance; true when one took them.
fn forward_to_running(files: &[String]) -> bool {
    unsafe {
        let Ok(hwnd) = FindWindowW(CLASS, PCWSTR::null()) else { return false };
        if hwnd.0.is_null() {
            return false;
        }
        let abs: Vec<String> = files
            .iter()
            .map(|f| std::path::absolute(f).map(|p| p.display().to_string()).unwrap_or_else(|_| f.clone()))
            .collect();
        let payload: Vec<u16> = abs.join("\n").encode_utf16().collect();
        let cds = COPYDATASTRUCT { dwData: 0x534C, cbData: (payload.len() * 2) as u32, lpData: payload.as_ptr() as *mut _ };
        SendMessageW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize));
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
        true
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Headless search hub: keeps the team's search index current.
    if args.iter().any(|a| a == "--hub") {
        settings::load();
        let rest: Vec<String> = args.iter().filter(|a| *a != "--hub").cloned().collect();
        std::process::exit(index::hub_main(&rest));
    }
    let mut files = Vec::new();
    let mut bench_ms = 0u32;
    let mut multi = false;
    let mut no_session = false;
    // --after-update <old window> <old version>: started by the updater.
    let mut handover: Option<(isize, String)> = None;
    let mut skip = 0;
    for (i, a) in args.iter().enumerate() {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        match a.as_str() {
            "--after-update" if i + 2 < args.len() => {
                handover = args[i + 1].parse().ok().map(|h| (h, args[i + 2].clone()));
                skip = 2;
            }
            "-multiInst" | "--new-instance" => multi = true,
            "-nosession" | "--no-session" => no_session = true,
            _ if i > 0 && i == args.len() - 1 && a.parse::<u32>().is_ok() && !std::path::Path::new(a).exists() => {
                bench_ms = a.parse().unwrap();
            }
            _ if a.starts_with('-') && !std::path::Path::new(a).exists() => {}
            _ => files.push(a.clone()),
        }
    }
    let bench = bench_ms > 0;
    if bench {
        multi = true;
        no_session = true;
    }
    unsafe {
        BENCH = bench;
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if !multi && forward_to_running(&files) {
        return;
    }
    settings::load();
    unsafe {
        let hinst = windows::Win32::Foundation::HINSTANCE::from(GetModuleHandleW(PCWSTR::null()).unwrap());
        let icon = LoadIconW(hinst, PCWSTR(1 as *const u16)).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hIcon: icon,
            hIconSm: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let place = settings::get().win;
        let (x, y, w, h) = match place {
            Some((x, y, w, h, _)) if w > 200 && h > 150 => (x, y, w, h),
            _ => (CW_USEDEFAULT, CW_USEDEFAULT, 1180, 780),
        };
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("Slate"), WS_OVERLAPPEDWINDOW, x, y, w, h, None, None, hinst, None).unwrap();
        let dpi = GetDpiForWindow(hwnd) as f32;
        let mut gfx = gfx::Gfx::new().expect("graphics");
        gfx.dark = settings::get().dark;
        gfx.accent = settings::get().accent;
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let _ = gfx.create_target(hwnd, rc.right as u32, rc.bottom as u32, dpi);
        let mut app = Box::new(App::new(hwnd, gfx));
        app.scale = dpi / 96.0;
        app.w = rc.right as f32 / app.scale;
        app.h = rc.bottom as f32 / app.scale;
        app.rebuild_fonts();
        app.no_session = no_session;
        if !bench {
            app.team_start();
        }
        if handover.is_some() || (!no_session && settings::get().restore_session) {
            app.restore_session();
        }
        if handover.is_none() && !bench {
            update::cleanup();
        }
        app.upd.handover = handover;
        for f in &files {
            app.open_path(f);
        }
        app.ensure_tab();
        if !files.is_empty() || app.views[app.av].tabs.len() > 0 {
            app.update_title();
        }
        app.reveal_caret();
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(app) as isize);
        // A 1 px frame margin keeps the DWM drop shadow on the borderless window.
        let m = windows::Win32::UI::Controls::MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 0, cyBottomHeight: 1 };
        let _ = windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea(hwnd, &m);
        if settings::get().acrylic {
            if let Some(a) = app_from(hwnd) {
                a.set_acrylic(true);
            }
        }
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER);
        DragAcceptFiles(hwnd, true);
        let maximized = matches!(place, Some((.., true)));
        let _ = ShowWindow(hwnd, if maximized { SW_SHOWMAXIMIZED } else { SW_SHOW });
        if bench {
            SetTimer(hwnd, T_BENCH, bench_ms, None);
        }
        if let Some(a) = app_from(hwnd) {
            a.finish_handover();
            a.schedule_update_checks();
            // The splash, except for benchmarks and an update taking over.
            if !bench && a.upd.handover.is_none() {
                a.splash_since = Some(std::time::Instant::now());
                a.redraw();
            }
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
