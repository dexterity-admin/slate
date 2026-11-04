//! Thin wrappers over Win32: clipboard, file dialogs, message boxes, menus.

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, POINT};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_ALLOWMULTISELECT, OFN_EXPLORER, OFN_FILEMUSTEXIST,
    OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, MessageBoxW, TrackPopupMenu, MB_ICONINFORMATION,
    MB_ICONWARNING, MB_OK, MB_YESNO, MB_YESNOCANCEL, MESSAGEBOX_RESULT, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, TPM_RETURNCMD, TPM_RIGHTBUTTON,
};

const CF_UNICODETEXT: u32 = 13;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn column_format() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("MSDEVColumnSelect")) }
}

/// Puts UTF-8 `text` on the clipboard; `column` marks a rectangular copy.
pub fn set_clipboard(hwnd: HWND, text: &[u8], column: bool) {
    let s: Vec<u16> = String::from_utf8_lossy(text).replace('\n', "\r\n").encode_utf16().chain([0]).collect();
    unsafe {
        if OpenClipboard(hwnd).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, s.len() * 2) {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
                let _ = GlobalUnlock(h);
                let _ = SetClipboardData(CF_UNICODETEXT, HANDLE(h.0));
            }
        }
        if column {
            if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, 1) {
                let _ = SetClipboardData(column_format(), HANDLE(h.0));
            }
        }
        let _ = CloseClipboard();
    }
}

/// Clipboard text as UTF-8 with `\n` line ends, and whether it was a column copy.
pub fn get_clipboard(hwnd: HWND) -> Option<(Vec<u8>, bool)> {
    unsafe {
        IsClipboardFormatAvailable(CF_UNICODETEXT).ok()?;
        let column = IsClipboardFormatAvailable(column_format()).is_ok();
        OpenClipboard(hwnd).ok()?;
        let mut out = None;
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT) {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if !p.is_null() {
                let mut n = 0;
                while *p.add(n) != 0 {
                    n += 1;
                }
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
                let _ = GlobalUnlock(g);
                let (t, _, _) = crate::enc::normalize_eol(s.into_bytes());
                out = Some((t, column));
            }
        }
        let _ = CloseClipboard();
        out
    }
}

const FILTER: &str = "All files (*.*)\0*.*\0Text (*.txt;*.log;*.md)\0*.txt;*.log;*.md\0Source code\0*.c;*.cpp;*.h;*.hpp;*.cs;*.rs;*.py;*.js;*.ts;*.java;*.go;*.ps1;*.bat;*.cmd;*.sh;*.sql\0Markup / data\0*.xml;*.html;*.htm;*.json;*.yml;*.yaml;*.ini;*.toml;*.csv\0\0";

pub fn open_dialog(hwnd: HWND, dir: Option<&str>) -> Vec<String> {
    let filter: Vec<u16> = FILTER.encode_utf16().collect();
    let mut buf = vec![0u16; 32768];
    let d = dir.map(wide);
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrInitialDir: d.as_ref().map(|v| PCWSTR(v.as_ptr())).unwrap_or(PCWSTR::null()),
        Flags: OFN_EXPLORER | OFN_ALLOWMULTISELECT | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST,
        ..Default::default()
    };
    if !unsafe { GetOpenFileNameW(&mut ofn) }.as_bool() {
        return Vec::new();
    }
    let parts: Vec<String> = buf
        .split(|&c| c == 0)
        .take_while(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect();
    if parts.len() <= 1 {
        return parts;
    }
    let base = std::path::PathBuf::from(&parts[0]);
    parts[1..].iter().map(|f| base.join(f).to_string_lossy().into_owned()).collect()
}

pub fn save_dialog(hwnd: HWND, suggested: &str, dir: Option<&str>) -> Option<String> {
    let filter: Vec<u16> = FILTER.encode_utf16().collect();
    let mut buf = vec![0u16; 4096];
    for (i, c) in suggested.encode_utf16().take(4000).enumerate() {
        buf[i] = c;
    }
    let d = dir.map(wide);
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrInitialDir: d.as_ref().map(|v| PCWSTR(v.as_ptr())).unwrap_or(PCWSTR::null()),
        Flags: OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST,
        ..Default::default()
    };
    if !unsafe { GetSaveFileNameW(&mut ofn) }.as_bool() {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&buf[..n]))
}

pub enum Answer {
    Yes,
    No,
    Cancel,
}

pub fn ask_save(hwnd: HWND, name: &str) -> Answer {
    let t = wide(&format!("Save changes to \"{name}\"?"));
    let r = unsafe { MessageBoxW(hwnd, PCWSTR(t.as_ptr()), w!("Slate"), MB_YESNOCANCEL | MB_ICONWARNING) };
    match r {
        MESSAGEBOX_RESULT(6) => Answer::Yes,
        MESSAGEBOX_RESULT(7) => Answer::No,
        _ => Answer::Cancel,
    }
}

pub fn ask_yes_no_cancel(hwnd: HWND, text: &str) -> Answer {
    let t = wide(text);
    let r = unsafe { MessageBoxW(hwnd, PCWSTR(t.as_ptr()), w!("Slate update"), MB_YESNOCANCEL | MB_ICONINFORMATION) };
    match r {
        MESSAGEBOX_RESULT(6) => Answer::Yes,
        MESSAGEBOX_RESULT(7) => Answer::No,
        _ => Answer::Cancel,
    }
}

pub fn ask_yes_no(hwnd: HWND, text: &str) -> bool {
    let t = wide(text);
    unsafe { MessageBoxW(hwnd, PCWSTR(t.as_ptr()), w!("Slate"), MB_YESNO | MB_ICONWARNING) == MESSAGEBOX_RESULT(6) }
}

pub fn info(hwnd: HWND, text: &str) {
    let t = wide(text);
    unsafe {
        MessageBoxW(hwnd, PCWSTR(t.as_ptr()), w!("Slate"), MB_OK | MB_ICONINFORMATION);
    }
}

/// Shows a popup menu of `(id, label, enabled)`; id 0 is a separator.
pub fn popup_menu(hwnd: HWND, screen: POINT, items: &[(u32, &str, bool)]) -> u32 {
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return 0 };
        for (id, label, en) in items {
            if *id == 0 {
                let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
            } else {
                let t = wide(label);
                let flags = if *en { MF_STRING } else { MF_STRING | MF_GRAYED };
                let _ = AppendMenuW(m, flags, *id as usize, PCWSTR(t.as_ptr()));
            }
        }
        let r = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, screen.x, screen.y, 0, hwnd, None);
        let _ = DestroyMenu(m);
        r.0 as u32
    }
}
