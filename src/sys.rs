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

const CF_UNICODETEXT: u32 = 13;

/// The window's backdrop: acrylic blur behind see-through parts (`moving`:
/// a plain blur while the window is dragged, as acrylic lags then on
/// Windows 10), or the normal opaque window.
pub fn set_backdrop(hwnd: HWND, on: bool, moving: bool, dark: bool) {
    use windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    #[repr(C)]
    struct Accent {
        state: u32,
        flags: u32,
        color: u32,
        anim: u32,
    }
    #[repr(C)]
    struct Data {
        attrib: u32,
        data: *mut Accent,
        size: usize,
    }
    type SetWca = unsafe extern "system" fn(HWND, *mut Data) -> i32;
    unsafe {
        // A one-pixel frame margin keeps the drop shadow. (Extending the
        // frame over the whole window would make Windows draw its own
        // caption buttons over Slate's; the accent alone is enough.)
        let m = windows::Win32::UI::Controls::MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 0, cyBottomHeight: 1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        let Ok(user32) = GetModuleHandleW(windows::core::w!("user32.dll")) else { return };
        let Some(f) = GetProcAddress(user32, windows::core::s!("SetWindowCompositionAttribute")) else { return };
        let f: SetWca = std::mem::transmute(f);
        // Gradient colour is ABGR: a light tint of the theme over the blur.
        let tint = if dark { 0x40201C18 } else { 0x40F8F6F4 };
        let mut a = Accent { state: if !on { 0 } else if moving { 3 } else { 4 }, flags: 2, color: tint, anim: 0 };
        let mut d = Data { attrib: 19, data: &mut a, size: std::mem::size_of::<Accent>() };
        f(hwnd, &mut d);
    }
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn column_format() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("MSDEVColumnSelect")) }
}

/// Puts UTF-8 `text` on the clipboard; `column` marks a rectangular copy.
pub fn set_clipboard(hwnd: HWND, text: &[u8], column: bool) {
    let s = String::from_utf8_lossy(text).replace('\n', "\r\n");
    let extra: &[(&str, Vec<u8>)] = if column { &[("MSDEVColumnSelect", Vec::new())] } else { &[] };
    set_clipboard_with(hwnd, &s, extra);
}

/// Puts `text` (line ends as given) and extra registered formats, each a
/// NUL-terminated byte string, such as "HTML Format" or "XML Spreadsheet".
pub fn set_clipboard_with(hwnd: HWND, text: &str, extra: &[(&str, Vec<u8>)]) {
    let s: Vec<u16> = text.encode_utf16().chain([0]).collect();
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
        for (name, data) in extra {
            let fmt = if *name == "MSDEVColumnSelect" { column_format() } else { RegisterClipboardFormatW(PCWSTR(wide(name).as_ptr())) };
            if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, data.len() + 1) {
                let p = GlobalLock(h) as *mut u8;
                if !p.is_null() {
                    std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
                    *p.add(data.len()) = 0;
                    let _ = GlobalUnlock(h);
                    let _ = SetClipboardData(fmt, HANDLE(h.0));
                }
            }
        }
        let _ = CloseClipboard();
    }
}

/// Copies table cells: tab-separated text plus the HTML and Excel formats,
/// so a spreadsheet pastes them as separate columns with values intact.
pub fn set_clipboard_table(hwnd: HWND, rows: &[Vec<String>]) {
    set_clipboard_with(hwnd, &crate::csv::to_tsv(rows), &table_formats(rows));
}

/// The spreadsheet formats for `rows` (none when the copy is very large).
pub fn table_formats(rows: &[Vec<String>]) -> Vec<(&'static str, Vec<u8>)> {
    let mut extra = Vec::new();
    if let Some(x) = crate::csv::to_xml_spreadsheet(rows) {
        extra.push(("XML Spreadsheet", x.into_bytes()));
    }
    if let Some(h) = crate::csv::to_html(rows) {
        extra.push(("HTML Format", h.into_bytes()));
    }
    extra
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

fn open_dialog_legacy(hwnd: HWND, dir: Option<&str>) -> Vec<String> {
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

fn save_dialog_legacy(hwnd: HWND, suggested: &str, dir: Option<&str>) -> Option<String> {
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

/// The file types offered by the Open and Save dialogs: (name, patterns).
const TYPES: [(&str, &str); 4] = [
    ("All files", "*.*"),
    ("Text", "*.txt;*.log;*.md"),
    ("Source code", "*.c;*.cpp;*.h;*.hpp;*.cs;*.rs;*.py;*.js;*.ts;*.java;*.go;*.ps1;*.bat;*.cmd;*.sh;*.sql"),
    ("Markup / data", "*.xml;*.html;*.htm;*.json;*.yml;*.yaml;*.ini;*.toml;*.csv"),
];

mod modern {
    use super::{wide, TYPES};
    use windows::core::{Interface, Result, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName,
        FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, SIGDN_FILESYSPATH,
    };

    fn path_of(item: &IShellItem) -> Result<String> {
        unsafe {
            let p = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            Ok(s)
        }
    }

    /// File types and start folder, shared by both dialogs.
    fn prepare(d: &IFileDialog, dir: Option<&str>) -> Result<()> {
        let names: Vec<(Vec<u16>, Vec<u16>)> = TYPES.iter().map(|(n, p)| (wide(n), wide(p))).collect();
        let specs: Vec<COMDLG_FILTERSPEC> = names.iter().map(|(n, p)| COMDLG_FILTERSPEC { pszName: PCWSTR(n.as_ptr()), pszSpec: PCWSTR(p.as_ptr()) }).collect();
        unsafe {
            d.SetFileTypes(&specs)?;
            if let Some(dir) = dir {
                let w = wide(dir);
                if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(w.as_ptr()), None) {
                    let _ = d.SetFolder(&item);
                }
            }
        }
        Ok(())
    }

    /// Ok(false) when the person cancelled; any other failure is an error,
    /// so the caller can fall back to the old dialog.
    fn shown(r: Result<()>) -> Result<bool> {
        const CANCELLED: i32 = 0x800704C7u32 as i32;
        match r {
            Ok(()) => Ok(true),
            Err(e) if e.code().0 == CANCELLED => Ok(false),
            Err(e) => {
                crate::trace(&format!("file dialog: {e}"));
                Err(e)
            }
        }
    }

    /// Ok(None) when the person cancelled.
    pub fn open(hwnd: HWND, dir: Option<&str>) -> Result<Option<Vec<String>>> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            d.SetOptions(d.GetOptions()? | FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST | FOS_FORCEFILESYSTEM)?;
            prepare(&d.cast()?, dir)?;
            if !shown(d.Show(hwnd))? {
                return Ok(None);
            }
            let items = d.GetResults()?;
            let mut out = Vec::new();
            for i in 0..items.GetCount()? {
                out.push(path_of(&items.GetItemAt(i)?)?);
            }
            Ok(Some(out))
        }
    }

    pub fn save(hwnd: HWND, suggested: &str, dir: Option<&str>) -> Result<Option<String>> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let d: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
            d.SetOptions(d.GetOptions()? | FOS_OVERWRITEPROMPT | FOS_PATHMUSTEXIST | FOS_FORCEFILESYSTEM)?;
            prepare(&d.cast()?, dir)?;
            let name = wide(suggested);
            d.SetFileName(PCWSTR(name.as_ptr()))?;
            if !shown(d.Show(hwnd))? {
                return Ok(None);
            }
            Ok(Some(path_of(&d.GetResult()?)?))
        }
    }
}

/// The Windows Explorer file dialog (with the old one as a fallback).
pub fn open_dialog(hwnd: HWND, dir: Option<&str>) -> Vec<String> {
    match modern::open(hwnd, dir) {
        Ok(v) => v.unwrap_or_default(),
        Err(_) => open_dialog_legacy(hwnd, dir),
    }
}

pub fn save_dialog(hwnd: HWND, suggested: &str, dir: Option<&str>) -> Option<String> {
    match modern::save(hwnd, suggested, dir) {
        Ok(v) => v,
        Err(_) => save_dialog_legacy(hwnd, suggested, dir),
    }
}

pub enum Answer {
    Yes,
    No,
    Cancel,
}

use crate::dialog::{self, Mark, Spec};

pub fn ask_save(hwnd: HWND, name: &str) -> Answer {
    let spec = Spec::new(Mark::Warn, &format!("Save changes to \u{201C}{name}\u{201D}?"), "Your changes are lost if you don\u{2019}t save them.").buttons(&["Save", "Don\u{2019}t save", "Cancel"], 0, 2);
    match dialog::show(hwnd, spec) {
        0 => Answer::Yes,
        1 => Answer::No,
        _ => Answer::Cancel,
    }
}

/// Asks before something that loses work: true for `yes`.
pub fn confirm(hwnd: HWND, title: &str, body: &str, yes: &str, no: &str) -> bool {
    dialog::show(hwnd, Spec::new(Mark::Warn, title, body).buttons(&[yes, no], 0, 1)) == 0
}

/// Reports a problem.
pub fn error(hwnd: HWND, title: &str, body: &str) {
    dialog::show(hwnd, Spec::new(Mark::Danger, title, body));
}

/// Shows a popup menu of `(id, label, enabled)`; id 0 is a separator and
/// a tab in a label puts its shortcut on the right.
pub fn popup_menu(hwnd: HWND, screen: POINT, items: &[(u32, &str, bool)]) -> u32 {
    crate::dialog::menu(hwnd, screen, items)
}
