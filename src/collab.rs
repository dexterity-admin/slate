//! Live co-editing through the team folder, with no server.
//!
//! A session is a folder `<team>\collab\<id>` holding the starting text
//! (`base`), an append-only `log` of operations and one cursor file per
//! person. The log is appended with the file opened exclusively, so its
//! order is the one order everybody agrees on. Each Slate replays the log
//! the same way (the ot.js server/client model): an operation written
//! against log position `base` is transformed over the entries after it.

use crate::app::App;
use crate::ot::Op;
use crate::team::{self, file_key};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted by a session worker with a `Box<Incoming>` in lParam.
pub const WM_COLLAB: u32 = WM_APP + 7;

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Op { client: String, base: usize, op: Op },
    Saved { client: String, rev: usize },
}

pub fn session_id(key: &str) -> String {
    let h = key.bytes().fold(0xcbf29ce484222325u64, |h, c| (h ^ c as u64).wrapping_mul(0x100000001b3));
    format!("{h:016x}")
}

pub fn encode_op(client: &str, base: usize, op: &Op) -> Vec<u8> {
    let payload = op.encode();
    let mut out = format!("O {client}\t{base}\t{}\n", payload.len()).into_bytes();
    out.extend_from_slice(&payload);
    out.push(b'\n');
    out
}

pub fn encode_saved(client: &str, rev: usize) -> Vec<u8> {
    format!("S {client}\t{rev}\n").into_bytes()
}

/// Complete entries at the start of `buf`, and the bytes they take. A
/// partly written entry at the end is left for the next read.
pub fn parse_entries(buf: &[u8]) -> Result<(Vec<Entry>, usize), String> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < buf.len() {
        let Some(nl) = crate::diff::find(&buf[at..], b'\n') else { break };
        let head = std::str::from_utf8(&buf[at..at + nl]).map_err(|_| "bad log header")?;
        let (kind, rest) = head.split_at(head.len().min(2));
        let f: Vec<&str> = rest.split('\t').collect();
        match (kind, f.as_slice()) {
            ("O ", [client, base, len]) => {
                let (base, len): (usize, usize) = (base.parse().map_err(|_| "bad base")?, len.parse().map_err(|_| "bad length")?);
                let s = at + nl + 1;
                if s + len + 1 > buf.len() {
                    break;
                }
                let op = Op::decode(&buf[s..s + len]).ok_or("bad operation")?;
                out.push(Entry::Op { client: client.to_string(), base, op });
                at = s + len + 1;
            }
            ("S ", [client, rev]) => {
                out.push(Entry::Saved { client: client.to_string(), rev: rev.parse().map_err(|_| "bad revision")? });
                at += nl + 1;
            }
            _ => return Err(format!("bad log entry: {head}")),
        }
    }
    Ok((out, at))
}

/// Someone else's caret in the shared text.
#[derive(Clone, Debug)]
pub struct Remote {
    pub client: String,
    pub rev: usize,
    pub head: usize,
}

pub struct Incoming {
    pub sid: String,
    pub entries: Vec<Entry>,
    pub cursors: Option<Vec<Remote>>,
    pub error: Option<String>,
}

enum Msg {
    Append(Vec<u8>),
    Cursor(String),
    Quit,
}

/// One buffer's part in a session.
pub struct Collab {
    pub sid: String,
    /// Every operation in log order, as everyone applies it.
    pub history: Vec<Op>,
    /// Sent and not yet seen in the log; local edits made meanwhile.
    pub awaiting: Option<Op>,
    pub pending: Option<Op>,
    /// Local changes not yet turned into an operation: (length before,
    /// start, end, new text).
    pub outbox: Vec<(usize, usize, usize, Vec<u8>)>,
    /// Set while a remote change is applied, so it is not sent back.
    pub muted: bool,
    pub cursors: Vec<Remote>,
    last_cursor: (usize, usize, usize),
    tx: mpsc::Sender<Msg>,
    wake: isize,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Collab {
    fn send(&self, m: Msg) {
        let _ = self.tx.send(m);
        unsafe {
            let _ = SetEvent(HANDLE(self.wake as *mut _));
        }
    }

    /// Where a position from log revision `rev` is in the local text.
    pub fn map_remote(&self, rev: usize, p: usize) -> usize {
        let mut p = p;
        for h in self.history.get(rev..).unwrap_or(&[]) {
            p = h.map_pos(p, false);
        }
        for o in [&self.awaiting, &self.pending].into_iter().flatten() {
            p = o.map_pos(p, false);
        }
        p
    }
}

impl Drop for Collab {
    fn drop(&mut self) {
        self.send(Msg::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe {
            let _ = CloseHandle(HANDLE(self.wake as *mut _));
        }
    }
}

/// Appends to the log with the file opened exclusively, retrying while
/// someone else is appending.
fn append(log: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    let mut tries = 0;
    loop {
        match std::fs::OpenOptions::new().append(true).create(true).share_mode(0).open(log) {
            Ok(mut f) => return f.write_all(bytes),
            Err(e) if e.raw_os_error() == Some(32) && tries < 250 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(8));
            }
            Err(e) => return Err(e),
        }
    }
}

fn read_from(log: &Path, offset: u64) -> std::io::Result<Vec<u8>> {
    use std::os::windows::fs::OpenOptionsExt;
    let mut tries = 0;
    loop {
        // Read while others may write: allow everything.
        match std::fs::OpenOptions::new().read(true).share_mode(7).open(log) {
            Ok(mut f) => {
                f.seek(SeekFrom::Start(offset))?;
                let mut v = Vec::new();
                f.read_to_end(&mut v)?;
                return Ok(v);
            }
            Err(e) if e.raw_os_error() == Some(32) && tries < 250 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(8));
            }
            Err(e) => return Err(e),
        }
    }
}

fn worker(dir: PathBuf, sid: String, client: String, mut offset: u64, rx: mpsc::Receiver<Msg>, wake: isize, hwnd: isize) {
    let log = dir.join("log");
    let my_cur = dir.join(format!("cur-{client}.txt"));
    let w: Vec<u16> = dir.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    let mut change = unsafe {
        FindFirstChangeNotificationW(PCWSTR(w.as_ptr()), false, FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE)
            .unwrap_or_default()
    };
    let mut cur_seen: HashMap<String, (SystemTime, u64)> = HashMap::new();
    let post = |inc: Incoming| {
        let p = Box::into_raw(Box::new(inc));
        unsafe {
            if PostMessageW(HWND(hwnd as *mut _), WM_COLLAB, WPARAM(0), LPARAM(p as isize)).is_err() {
                drop(Box::from_raw(p));
            }
        }
    };
    loop {
        let mut error = None;
        let mut cursor = None;
        let mut quit = false;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Append(b) => {
                    if let Err(e) = append(&log, &b) {
                        error = Some(format!("Could not write to the shared session: {e}"));
                    }
                }
                // Only the latest caret matters.
                Msg::Cursor(c) => cursor = Some(c),
                Msg::Quit => quit = true,
            }
        }
        if quit {
            let _ = std::fs::remove_file(&my_cur);
            if !change.0.is_null() && !change.is_invalid() {
                unsafe {
                    let _ = FindCloseChangeNotification(change);
                }
            }
            return;
        }
        if let Some(c) = cursor {
            let tmp = dir.join(format!("cur-{client}.tmp"));
            if std::fs::write(&tmp, c).is_ok() {
                let _ = std::fs::rename(&tmp, &my_cur);
            }
        }
        let mut entries = Vec::new();
        match read_from(&log, offset) {
            Ok(bytes) => match parse_entries(&bytes) {
                Ok((e, used)) => {
                    offset += used as u64;
                    entries = e;
                }
                Err(e) => error = Some(format!("The shared session is damaged ({e})")),
            },
            Err(e) => {
                if error.is_none() {
                    error = Some(format!("Cannot read the shared session: {e}"));
                }
            }
        }
        // Others' carets.
        let mut cursors = None;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut changed = false;
            let mut list = Vec::new();
            let mut present = Vec::new();
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let Some(c) = name.strip_prefix("cur-").and_then(|n| n.strip_suffix(".txt")) else { continue };
                if c == client {
                    continue;
                }
                present.push(c.to_string());
                let md = e.metadata().ok();
                let key = (md.as_ref().and_then(|m| m.modified().ok()).unwrap_or(UNIX_EPOCH), md.map(|m| m.len()).unwrap_or(0));
                if cur_seen.get(c) != Some(&key) {
                    changed = true;
                    cur_seen.insert(c.to_string(), key);
                }
                if let Ok(t) = std::fs::read_to_string(e.path()) {
                    let v: Vec<usize> = t.trim().split('\t').filter_map(|x| x.parse().ok()).collect();
                    if v.len() == 3 {
                        // Fields: revision, anchor (kept for selections later), head.
                        list.push(Remote { client: c.to_string(), rev: v[0], head: v[2] });
                    }
                }
            }
            let n = cur_seen.len();
            cur_seen.retain(|k, _| present.contains(k));
            if changed || n != cur_seen.len() {
                cursors = Some(list);
            }
        }
        if !entries.is_empty() || cursors.is_some() || error.is_some() {
            post(Incoming { sid: sid.clone(), entries, cursors, error });
        }
        let handles: Vec<HANDLE> = [HANDLE(wake as *mut _), change].into_iter().filter(|h| !h.0.is_null() && !h.is_invalid()).collect();
        let r = unsafe { WaitForMultipleObjects(&handles, false, if handles.len() == 2 { 10_000 } else { 400 }) };
        if handles.len() == 2 && r.0 == 1 {
            unsafe {
                if FindNextChangeNotification(change).is_err() {
                    let _ = FindCloseChangeNotification(change);
                    change = HANDLE::default();
                }
            }
        }
    }
}

impl App {
    /// Sessions this Slate takes part in (published with presence).
    pub fn collab_ids(&self) -> Vec<String> {
        self.bufs.iter().filter_map(|b| b.collab.as_ref().map(|c| c.sid.clone())).collect()
    }

    fn session_of(&self, bi: usize) -> Option<(String, PathBuf)> {
        let t = self.team.as_ref()?;
        let p = self.bufs[bi].path.as_ref()?;
        let sid = session_id(&file_key(&t.shared(p)));
        let dir = t.dir.join("collab").join(&sid);
        Some((sid, dir))
    }

    /// Others in a session.
    pub fn collab_people(&self, sid: &str) -> Vec<String> {
        let Some(t) = &self.team else { return Vec::new() };
        let mut v: Vec<String> = t.peers.iter().filter(|p| p.id != t.me && p.collab.iter().any(|c| c == sid)).map(|p| p.name.clone()).collect();
        v.dedup();
        v
    }

    /// Ctrl+Shift+E: start, join or leave co-editing the current file.
    pub fn collab_toggle(&mut self) {
        let bi = self.bi(self.tab(self.av).buf);
        if self.bufs[bi].collab.is_some() {
            self.bufs[bi].collab = None;
            self.team_publish();
            return self.flash("You left co-editing; the file stays open with the shared text");
        }
        if self.team.is_none() {
            return self.flash("Co-editing needs a team folder \u{2013} Team: Set Team Folder");
        }
        if self.bufs[bi].path.is_none() || self.bufs[bi].diff.is_some() || self.bufs[bi].loading.is_some() {
            return self.flash("Co-editing works on saved files in a shared folder");
        }
        if let Some(by) = self.bufs[bi].locked_by.clone() {
            return self.flash(&format!("{by} has locked this file"));
        }
        let (sid, dir) = self.session_of(bi).unwrap();
        let others = self.collab_people(&sid);
        let r = if others.is_empty() { self.collab_start(bi, &sid, &dir) } else { self.collab_join(bi, &sid, &dir, &others) };
        match r {
            Ok(msg) => {
                self.bufs[bi].lock_since = None;
                self.team_publish();
                self.flash(&msg);
            }
            Err(e) => self.flash(&e),
        }
        self.after_switch();
    }

    fn collab_attach(&mut self, bi: usize, sid: &str, dir: &Path, history: Vec<Op>, offset: u64) {
        let me = self.team.as_ref().unwrap().me.clone();
        let (tx, rx) = mpsc::channel();
        let wake = unsafe { CreateEventW(None, false, false, None).map(|h| h.0 as isize).unwrap_or(0) };
        let (d, s, c, hw) = (dir.to_path_buf(), sid.to_string(), me, self.hwnd.0 as isize);
        let thread = std::thread::spawn(move || worker(d, s, c, offset, rx, wake, hw));
        self.bufs[bi].collab = Some(Box::new(Collab {
            sid: sid.to_string(),
            history,
            awaiting: None,
            pending: None,
            outbox: Vec::new(),
            muted: false,
            cursors: Vec::new(),
            last_cursor: (usize::MAX, 0, 0),
            tx,
            wake,
            thread: Some(thread),
        }));
        self.collab_cursor();
    }

    fn collab_start(&mut self, bi: usize, sid: &str, dir: &Path) -> Result<String, String> {
        let err = |e: std::io::Error| format!("Cannot start co-editing: {e}");
        std::fs::create_dir_all(dir).map_err(err)?;
        // Someone may have started this a moment ago (presence lags a little).
        let meta = dir.join("meta.txt");
        if let Ok(m) = std::fs::metadata(&meta) {
            let fresh = m.modified().ok().and_then(|t| t.elapsed().ok()).map_or(false, |d| d.as_secs() < 30);
            if fresh {
                return Err("Someone is starting co-editing on this file right now \u{2013} try again in a few seconds".into());
            }
        }
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        let text = self.bufs[bi].text_all();
        let t = self.team.as_ref().unwrap();
        let path = t.shared(self.bufs[bi].path.as_ref().unwrap());
        std::fs::write(dir.join("base"), &text).map_err(err)?;
        std::fs::write(dir.join("log"), b"").map_err(err)?;
        std::fs::write(&meta, format!("path={path}\nby={}\nstarted={}\n", t.name, team::now())).map_err(err)?;
        self.collab_attach(bi, sid, dir, Vec::new(), 0);
        Ok("Co-editing on: others who open this file can join with Ctrl+Shift+E".into())
    }

    fn collab_join(&mut self, bi: usize, sid: &str, dir: &Path, others: &[String]) -> Result<String, String> {
        if self.bufs[bi].dirty() && !crate::sys::ask_yes_no(self.hwnd, "Join co-editing?\n\nYour unsaved changes to this file are replaced by the shared text.") {
            return Err("Not joined".into());
        }
        let err = |e: std::io::Error| format!("Cannot join co-editing: {e}");
        let mut text = std::fs::read(dir.join("base")).map_err(err)?;
        let log = read_from(&dir.join("log"), 0).map_err(err)?;
        let (entries, used) = parse_entries(&log)?;
        let mut history: Vec<Op> = Vec::new();
        for e in entries {
            if let Entry::Op { base, op, .. } = e {
                let c = canonical(&history, base, op).ok_or("The shared session is damaged")?;
                text = c.apply(&text).ok_or("The shared session is damaged")?;
                history.push(c);
            }
        }
        let b = &mut self.bufs[bi];
        let caret = b.caret();
        b.doc = crate::doc::Doc::load(text);
        b.gen += 1;
        b.states.clear();
        b.folds.clear();
        b.set_caret(caret);
        b.mark_unsaved();
        if let Some(mut g) = b.grid.take() {
            g.reindex(&mut b.doc, 0);
            b.grid = Some(g);
        }
        self.collab_attach(bi, sid, dir, history, used as u64);
        Ok(format!("Co-editing with {}", team::join_names(&others.iter().map(|s| s.as_str()).collect::<Vec<_>>())))
    }

    /// Sends local edits made since the last call.
    pub fn collab_flush(&mut self) {
        let me = match &self.team {
            Some(t) => t.me.clone(),
            None => return,
        };
        for b in self.bufs.iter_mut() {
            let Some(c) = b.collab.as_mut() else { continue };
            if c.outbox.is_empty() {
                continue;
            }
            let mut op: Option<Op> = None;
            for (len, a, z, t) in c.outbox.drain(..) {
                let o = Op::replace(len, a, z, &t);
                op = Some(match op {
                    Some(p) => p.compose(&o).unwrap_or(o),
                    None => o,
                });
            }
            let Some(op) = op else { continue };
            if c.awaiting.is_none() {
                c.send(Msg::Append(encode_op(&me, c.history.len(), &op)));
                c.awaiting = Some(op);
            } else {
                c.pending = Some(match c.pending.take() {
                    Some(p) => p.compose(&op).unwrap_or(op),
                    None => op,
                });
            }
        }
        self.collab_cursor();
    }

    /// Publishes the caret of the current co-edited buffer when it moved.
    pub fn collab_cursor(&mut self) {
        let bi = self.bi(self.tab(self.av).buf);
        let b = &mut self.bufs[bi];
        let s = b.primary();
        let Some(c) = b.collab.as_mut() else { return };
        // Positions are sent in log coordinates when nothing is in flight.
        if c.awaiting.is_some() {
            return;
        }
        let now = (c.history.len(), s.anchor, s.head);
        if now != c.last_cursor {
            c.last_cursor = now;
            c.send(Msg::Cursor(format!("{}\t{}\t{}\n", now.0, now.1, now.2)));
        }
    }

    pub fn collab_saved(&mut self, id: u32) {
        let me = match &self.team {
            Some(t) => t.me.clone(),
            None => return,
        };
        let bi = self.bi(id);
        if let Some(c) = self.bufs[bi].collab.as_ref() {
            if c.awaiting.is_none() && c.pending.is_none() {
                c.send(Msg::Append(encode_saved(&me, c.history.len())));
            }
        }
    }

    pub fn on_collab(&mut self, inc: Incoming) {
        let Some(bi) = self.bufs.iter().position(|b| b.collab.as_ref().map_or(false, |c| c.sid == inc.sid)) else { return };
        let me = self.team.as_ref().map(|t| t.me.clone()).unwrap_or_default();
        if let Some(e) = inc.error {
            self.bufs[bi].collab = None;
            self.team_publish();
            self.flash(&format!("{e} \u{2013} co-editing stopped; your text is kept"));
            return self.redraw();
        }
        let mut changed = false;
        for e in inc.entries {
            let b = &mut self.bufs[bi];
            let c = b.collab.as_mut().unwrap();
            match e {
                Entry::Op { client, .. } if client == me => {
                    // Our own operation came back: acknowledged.
                    if let Some(aw) = c.awaiting.take() {
                        c.history.push(aw);
                    }
                    if let Some(p) = c.pending.take() {
                        c.send(Msg::Append(encode_op(&me, c.history.len(), &p)));
                        c.awaiting = Some(p);
                    }
                }
                Entry::Op { base, op, .. } => {
                    let Some(canon) = canonical(&c.history, base, op) else {
                        b.collab = None;
                        self.flash("The shared session is out of step \u{2013} co-editing stopped; your text is kept");
                        self.team_publish();
                        return self.redraw();
                    };
                    c.history.push(canon.clone());
                    let mut x = canon;
                    for slot in [&mut c.awaiting, &mut c.pending] {
                        if let Some(o) = slot.take() {
                            match Op::transform(&o, &x) {
                                Some((o2, x2)) => {
                                    *slot = Some(o2);
                                    x = x2;
                                }
                                None => *slot = Some(o),
                            }
                        }
                    }
                    if x.base == b.doc.len() {
                        b.apply_remote(&x.changes());
                        changed = true;
                    }
                }
                Entry::Saved { client, rev } => {
                    if client != me && c.awaiting.is_none() && c.pending.is_none() && rev == c.history.len() {
                        b.mark_saved();
                        b.mtime = b.path.as_ref().and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
                    }
                }
            }
        }
        if let Some(list) = inc.cursors {
            if let Some(c) = self.bufs[bi].collab.as_mut() {
                c.cursors = list;
            }
        }
        if changed {
            self.edited();
            self.recount_if_open();
        }
        self.update_title();
        self.redraw();
    }

    /// Offers to join when someone starts co-editing a file open here.
    pub fn collab_on_team(&mut self) {
        let mut offers = Vec::new();
        for bi in 0..self.bufs.len() {
            if self.bufs[bi].collab.is_some() || self.bufs[bi].path.is_none() {
                continue;
            }
            let Some((sid, _)) = self.session_of(bi) else { continue };
            let people = self.collab_people(&sid);
            let offered = self.collab_offered.get(&sid).cloned();
            if !people.is_empty() && offered.as_deref() != Some(&people[0]) {
                offers.push((sid.clone(), people.clone(), self.bufs[bi].name()));
            } else if people.is_empty() {
                self.collab_offered.remove(&sid);
            }
        }
        for (sid, people, name) in offers {
            self.collab_offered.insert(sid, people[0].clone());
            let names: Vec<&str> = people.iter().map(|s| s.as_str()).collect();
            self.flash(&format!("{} {} co-editing {name} \u{2013} Ctrl+Shift+E joins", team::join_names(&names), if names.len() == 1 { "is" } else { "are" }));
        }
        // Remote carets whose owner left.
        for b in self.bufs.iter_mut() {
            if let (Some(c), Some(t)) = (b.collab.as_mut(), self.team.as_ref()) {
                c.cursors.retain(|r| t.peers.iter().any(|p| p.id == r.client));
            }
        }
    }

    /// Other people's carets in buffer `bi`: (position, name, colour).
    pub fn collab_carets(&self, bi: usize) -> Vec<(usize, String, u32)> {
        let (Some(c), Some(t)) = (self.bufs[bi].collab.as_ref(), self.team.as_ref()) else { return Vec::new() };
        let len = self.bufs[bi].doc.len();
        c.cursors
            .iter()
            .filter_map(|r| {
                let p = t.peers.iter().find(|p| p.id == r.client)?;
                Some((c.map_remote(r.rev, r.head).min(len), p.name.clone(), p.color()))
            })
            .collect()
    }
}

/// An operation written against log position `base`, as it applies after
/// every entry before it.
pub fn canonical(history: &[Op], base: usize, op: Op) -> Option<Op> {
    let mut o = op;
    for h in history.get(base..)? {
        o = Op::transform(&o, h)?.0;
    }
    Some(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulates clients sharing one log: each sends operations against
    /// what it has seen, and everyone must end with the same text.
    #[test]
    fn clients_converge_through_the_log() {
        struct Client {
            text: Vec<u8>,
            history: Vec<Op>,
            seen: usize,
            awaiting: Option<Op>,
            pending: Option<Op>,
        }
        let base = b"PIPE\nELBOW\nFLANGE\n".to_vec();
        let mut log: Vec<u8> = Vec::new();
        let mut cl: Vec<Client> = (0..3).map(|_| Client { text: base.clone(), history: vec![], seen: 0, awaiting: None, pending: None }).collect();
        let mut seed = 12345u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as usize
        };
        for step in 0..400 {
            let k = rnd() % 3;
            if rnd() % 3 != 0 {
                // Local edit.
                let c = &mut cl[k];
                let len = c.text.len();
                let a = rnd() % (len + 1);
                let z = (a + rnd() % 3).min(len);
                let ins = format!("{k}{}", step % 10).into_bytes();
                let op = Op::replace(len, a, z, &ins);
                c.text = op.apply(&c.text).unwrap();
                if c.awaiting.is_none() {
                    log.extend(encode_op(&format!("c{k}"), c.history.len(), &op));
                    c.awaiting = Some(op);
                } else {
                    c.pending = Some(match c.pending.take() {
                        Some(p) => p.compose(&op).unwrap(),
                        None => op,
                    });
                }
            } else {
                // Read the log.
                let (entries, _) = parse_entries(&log).unwrap();
                let c = &mut cl[k];
                for e in entries.into_iter().skip(c.seen) {
                    c.seen += 1;
                    let Entry::Op { client, base, op } = e else { continue };
                    if client == format!("c{k}") {
                        c.history.push(c.awaiting.take().unwrap());
                        if let Some(p) = c.pending.take() {
                            log.extend(encode_op(&client, c.history.len(), &p));
                            c.awaiting = Some(p);
                        }
                    } else {
                        let canon = canonical(&c.history, base, op).unwrap();
                        c.history.push(canon.clone());
                        let mut x = canon;
                        for slot in [&mut c.awaiting, &mut c.pending] {
                            if let Some(o) = slot.take() {
                                let (o2, x2) = Op::transform(&o, &x).unwrap();
                                *slot = Some(o2);
                                x = x2;
                            }
                        }
                        c.text = x.apply(&c.text).unwrap();
                    }
                }
            }
        }
        // Drain: everyone reads until nothing is in flight.
        for _ in 0..10 {
            for k in 0..3 {
                let (entries, _) = parse_entries(&log).unwrap();
                let c = &mut cl[k];
                for e in entries.into_iter().skip(c.seen) {
                    c.seen += 1;
                    let Entry::Op { client, base, op } = e else { continue };
                    if client == format!("c{k}") {
                        c.history.push(c.awaiting.take().unwrap());
                        if let Some(p) = c.pending.take() {
                            log.extend(encode_op(&client, c.history.len(), &p));
                            c.awaiting = Some(p);
                        }
                    } else {
                        let canon = canonical(&c.history, base, op).unwrap();
                        c.history.push(canon.clone());
                        let mut x = canon;
                        for slot in [&mut c.awaiting, &mut c.pending] {
                            if let Some(o) = slot.take() {
                                let (o2, x2) = Op::transform(&o, &x).unwrap();
                                *slot = Some(o2);
                                x = x2;
                            }
                        }
                        c.text = x.apply(&c.text).unwrap();
                    }
                }
            }
        }
        assert!(cl.iter().all(|c| c.awaiting.is_none() && c.pending.is_none()));
        assert_eq!(cl[0].text, cl[1].text);
        assert_eq!(cl[1].text, cl[2].text);
        // And it equals replaying the log from scratch.
        let (entries, _) = parse_entries(&log).unwrap();
        let mut h = Vec::new();
        let mut t = base.clone();
        for e in entries {
            if let Entry::Op { base, op, .. } = e {
                let c = canonical(&h, base, op).unwrap();
                t = c.apply(&t).unwrap();
                h.push(c);
            }
        }
        assert_eq!(t, cl[0].text);
    }

    #[test]
    fn partial_entries_wait() {
        let mut log = encode_op("a", 0, &Op::replace(0, 0, 0, b"hi\n"));
        log.extend(encode_saved("a", 1));
        let full = log.len();
        log.extend(&encode_op("b", 1, &Op::replace(3, 3, 3, b"x"))[..5]);
        let (e, used) = parse_entries(&log).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(used, full);
    }
}
