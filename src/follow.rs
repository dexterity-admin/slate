//! Follow mode ("tail -f"): a log file's new lines appear as they are
//! written. A worker thread keeps the file open (sharing it with the
//! program writing it), waits for change notifications on its folder and
//! also checks the size once a second, because Windows does not always
//! report growth of a file someone else holds open. New bytes are decoded
//! with the buffer's encoding and appended without an undo step; a file
//! that shrinks (rotated or truncated) is reloaded.

use crate::app::{App, Cmd};
use crate::enc::{Enc, StreamDecoder};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted with a `Box<Grown>` in lParam.
pub const WM_FOLLOW: u32 = WM_APP + 9;

pub enum Grown {
    /// New text at the end, with its line starts relative to the piece.
    More { id: u32, text: Vec<u8>, starts: Vec<u32> },
    /// The file got shorter: it was rotated or truncated.
    Reset { id: u32 },
    Gone { id: u32, why: String },
}

/// A followed buffer's worker; dropping it stops the worker.
pub struct Follow {
    stop: Arc<AtomicBool>,
}

impl Drop for Follow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn post(hwnd: isize, g: Grown) -> bool {
    let p = Box::into_raw(Box::new(g));
    unsafe {
        if PostMessageW(HWND(hwnd as *mut _), WM_FOLLOW, WPARAM(0), LPARAM(p as isize)).is_err() {
            drop(Box::from_raw(p));
            return false;
        }
    }
    true
}

/// Opens a file for reading while another program writes (and maybe
/// renames or deletes) it.
fn open_shared(p: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().read(true).share_mode(7).open(p)
}

fn watch(path: PathBuf, id: u32, mut offset: u64, enc: Enc, ends_cr: bool, stop: Arc<AtomicBool>, hwnd: isize) {
    let dir = path.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let w: Vec<u16> = dir.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    let change = unsafe {
        FindFirstChangeNotificationW(PCWSTR(w.as_ptr()), false, FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME).ok()
    };
    let mut dec = StreamDecoder::resume(enc, ends_cr);
    let mut file = open_shared(&path).ok();
    let mut buf = vec![0u8; 4 << 20];
    while !stop.load(Ordering::Relaxed) {
        // The size through the open handle sees growth at once; the size by
        // name sees a rotation (a new, shorter file under the same name).
        let by_name = std::fs::metadata(&path).map(|m| m.len()).ok();
        let Some(by_name) = by_name else {
            if !post(hwnd, Grown::Gone { id, why: "the file is gone".into() }) {
                break;
            }
            break;
        };
        let by_handle = file.as_ref().and_then(|f| f.metadata().ok()).map(|m| m.len()).unwrap_or(by_name);
        if by_name < offset {
            if !post(hwnd, Grown::Reset { id }) {
                break;
            }
            break;
        }
        let size = by_handle.max(by_name);
        if size > offset {
            if file.is_none() {
                file = open_shared(&path).ok();
            }
            let Some(f) = file.as_mut() else { break };
            if f.seek(SeekFrom::Start(offset)).is_err() {
                file = None;
                continue;
            }
            let mut left = size - offset;
            while left > 0 && !stop.load(Ordering::Relaxed) {
                let want = (left as usize).min(buf.len());
                let n = match f.read(&mut buf[..want]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                offset += n as u64;
                left -= n as u64;
                // Never "last": the file may go on growing.
                let text = dec.push(&buf[..n], false).unwrap_or_else(|_| String::from_utf8_lossy(&buf[..n]).into_owned().into_bytes());
                if !text.is_empty() {
                    let mut starts = Vec::new();
                    crate::doc::each_byte(&text, b'\n', |i| starts.push((i + 1) as u32));
                    if !post(hwnd, Grown::More { id, text, starts }) {
                        return;
                    }
                }
            }
        }
        match change {
            Some(h) => unsafe {
                // A change in the folder, or a second at most.
                if WaitForSingleObject(h, 1000).0 == 0 {
                    let _ = FindNextChangeNotification(h);
                }
            },
            None => std::thread::sleep(Duration::from_millis(1000)),
        }
    }
    if let Some(h) = change {
        unsafe {
            let _ = FindCloseChangeNotification(h);
        }
    }
}

impl App {
    /// Starts or stops following the current file.
    pub fn follow_toggle(&mut self) {
        let bi = self.bi(self.tab(self.av).buf);
        if self.bufs[bi].follow.is_some() {
            self.bufs[bi].follow = None;
            self.flash("Stopped following \u{2013} the file can be edited again");
            return self.redraw();
        }
        self.follow_start(bi);
    }

    fn follow_start(&mut self, bi: usize) {
        let b = &self.bufs[bi];
        let Some(path) = b.path.clone() else { return self.flash("Save the file first") };
        if b.loading.is_some() || b.saving.is_some() {
            return self.flash("Wait until the file has finished loading");
        }
        if b.dirty() {
            return self.flash("Save or undo your changes first: following shows the file as it is on disk");
        }
        if b.collab.is_some() || b.diff.is_some() {
            return self.flash("A co-edited or compared file cannot be followed");
        }
        let Ok(md) = std::fs::metadata(&path) else { return self.flash("Cannot read the file") };
        let offset = md.len();
        // Whether the last byte read was a carriage return (half of CR LF).
        let ends_cr = offset > 0
            && open_shared(&path).ok().and_then(|mut f| {
                f.seek(SeekFrom::Start(offset - 1)).ok()?;
                let mut c = [0u8];
                f.read_exact(&mut c).ok()?;
                Some(c[0] == b'\r')
            }) == Some(true);
        let stop = Arc::new(AtomicBool::new(false));
        let (id, enc, hwnd) = (b.id, b.enc, self.hwnd.0 as isize);
        let s2 = stop.clone();
        std::thread::Builder::new().name("slate-follow".into()).stack_size(256 * 1024).spawn(move || watch(path, id, offset, enc, ends_cr, s2, hwnd)).ok();
        let b = &mut self.bufs[bi];
        b.follow = Some(Follow { stop });
        // Following starts at the end.
        let end = b.doc.len();
        b.set_caret(end);
        self.reveal_caret();
        self.flash("Following: new lines appear as they are written (read-only meanwhile)");
        self.redraw();
    }

    pub fn on_follow(&mut self, g: Grown) {
        match g {
            Grown::More { id, text, starts } => {
                let Some(bi) = self.bufs.iter().position(|b| b.id == id && b.follow.is_some()) else { return };
                let at_end = {
                    let b = &self.bufs[bi];
                    b.caret() == b.doc.len()
                };
                let b = &mut self.bufs[bi];
                b.doc.append_loaded(&text, &starts);
                b.gen += 1;
                b.mtime = b.path.as_ref().and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
                // Keep to the end, unless you scrolled up to read.
                if at_end {
                    let end = b.doc.len();
                    b.set_caret(end);
                    let shown = self.views.iter().enumerate().any(|(v, vw)| vw.tabs.get(vw.active).map_or(false, |t| t.buf == id) && (v == 0 || self.split));
                    if shown && self.tab(self.av).buf == id {
                        self.reveal_caret();
                    }
                }
                self.redraw();
            }
            Grown::Reset { id } => {
                let Some(bi) = self.bufs.iter().position(|b| b.id == id && b.follow.is_some()) else { return };
                self.bufs[bi].follow = None;
                self.reload(id, None);
                let bi = self.bi(id);
                self.follow_start(bi);
                self.flash("The log was rotated or truncated: reloaded, still following");
            }
            Grown::Gone { id, why } => {
                if let Some(bi) = self.bufs.iter().position(|b| b.id == id) {
                    self.bufs[bi].follow = None;
                }
                self.flash(&format!("Stopped following: {why}"));
            }
        }
    }

    /// Status-bar click on "Follow" / "Following".
    pub fn follow_status_click(&mut self) {
        self.exec(Cmd::Follow);
    }
}
