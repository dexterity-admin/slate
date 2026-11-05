//! Team features over a shared folder, with no server and no inbound
//! network ports: presence (who is online and which shared files they have
//! open), open-file warnings and edit locks.
//!
//! Each running Slate keeps one small text file in `<team folder>\presence`
//! and a worker thread watches that folder for everyone else's. Paths on
//! mapped drives are published as UNC paths so that every PC names a file
//! the same way.

use crate::app::{App, PalAct, PalItem, PalMode};
use crate::settings;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted by the worker with a `Box<Vec<Peer>>` in lParam.
pub const WM_TEAM: u32 = WM_APP + 6;
/// Own presence is rewritten this often even when nothing changed.
const BEAT_SECS: u64 = 60;
/// A presence file not refreshed for this long belongs to a PC that went
/// away without closing Slate.
const STALE_SECS: u64 = 300;
/// Stale presence files older than this are deleted by whoever sees them.
const PURGE_SECS: u64 = 86_400;

/// Colours people are drawn in, picked from their name.
pub const COLORS: [u32; 8] = [0x58A6FF, 0x3FB950, 0xD29922, 0xF778BA, 0xA371F7, 0x39C5CF, 0xF0883E, 0xE5534B];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileState {
    Open,
    /// Has unsaved changes.
    Editing,
    Locked,
}

#[derive(Clone, Debug)]
pub struct PeerFile {
    /// Comparison key: the shared path in lower case.
    pub key: String,
    pub path: String,
    pub state: FileState,
    pub since: u64,
}

#[derive(Clone, Debug)]
pub struct Peer {
    /// Presence file stem: unique per running Slate.
    pub id: String,
    pub name: String,
    pub pc: String,
    pub beat: u64,
    pub files: Vec<PeerFile>,
    /// Co-editing sessions joined.
    pub collab: Vec<String>,
}

impl Peer {
    pub fn color(&self) -> u32 {
        color_for(&self.name)
    }
    pub fn initials(&self) -> String {
        initials(&self.name)
    }
    pub fn file(&self, key: &str) -> Option<&PeerFile> {
        self.files.iter().find(|f| f.key == key)
    }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn color_for(name: &str) -> u32 {
    let h = name.to_lowercase().bytes().fold(7u32, |h, c| h.wrapping_mul(31).wrapping_add(c as u32));
    COLORS[h as usize % COLORS.len()]
}

pub fn initials(name: &str) -> String {
    let mut it = name.split_whitespace();
    let a = it.next().and_then(|w| w.chars().next());
    let b = it.last().and_then(|w| w.chars().next());
    match (a, b) {
        (Some(a), Some(b)) => format!("{}{}", a.to_uppercase(), b.to_uppercase()),
        (Some(a), None) => {
            let mut s: String = name.chars().take(2).collect();
            if s.is_empty() {
                s.push(a);
            }
            let mut c = s.chars();
            let first = c.next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            first + &c.as_str().to_lowercase()
        }
        _ => "?".into(),
    }
}

/// The name shown to others: setting, else the Windows user name.
pub fn my_name() -> String {
    if let Ok(n) = std::env::var("SLATE_TEAM_NAME") {
        return n;
    }
    let s = settings::get().team_name.clone();
    if !s.trim().is_empty() {
        return s.trim().to_string();
    }
    let u = std::env::var("USERNAME").unwrap_or_else(|_| "Someone".into());
    let mut c = u.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => u,
    }
}

/// Team folder: setting, or SLATE_TEAM_DIR for testing. Empty means the
/// team features are off.
pub fn team_dir() -> Option<PathBuf> {
    let d = std::env::var("SLATE_TEAM_DIR").ok().unwrap_or_else(|| settings::get().team_dir.clone());
    let d = d.trim().trim_matches('"').to_string();
    (!d.is_empty()).then(|| PathBuf::from(d))
}

pub fn file_key(p: &str) -> String {
    p.replace('/', "\\").to_lowercase()
}

/// The path as other PCs see it: a mapped drive letter becomes its UNC
/// path. Local paths come back unchanged; they only match on the same PC.
pub fn shared_path(p: &Path) -> String {
    use windows::Win32::NetworkManagement::WNet::{WNetGetUniversalNameW, UNIVERSAL_NAME_INFOW, UNIVERSAL_NAME_INFO_LEVEL};
    let s = p.to_string_lossy().into_owned();
    if s.starts_with("\\\\") {
        return s;
    }
    let w: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u8; 2048];
    let mut len = buf.len() as u32;
    let r = unsafe { WNetGetUniversalNameW(PCWSTR(w.as_ptr()), UNIVERSAL_NAME_INFO_LEVEL, buf.as_mut_ptr() as *mut _, &mut len) };
    if r.0 != 0 {
        return s;
    }
    unsafe {
        let info = &*(buf.as_ptr() as *const UNIVERSAL_NAME_INFOW);
        info.lpUniversalName.to_string().unwrap_or(s)
    }
}

pub fn render(name: &str, pc: &str, files: &[PeerFile], collab: &[String], beat: u64) -> String {
    let mut out = format!("slate-presence 1\nname={name}\npc={pc}\nbeat={beat}\n");
    for f in files {
        let st = match f.state {
            FileState::Open => "open",
            FileState::Editing => "edit",
            FileState::Locked => "lock",
        };
        out += &format!("file={st}\t{}\t{}\n", f.since, f.path);
    }
    for c in collab {
        out += &format!("collab={c}\n");
    }
    out
}

pub fn parse(id: &str, text: &str) -> Option<Peer> {
    let mut lines = text.lines();
    if lines.next()? != "slate-presence 1" {
        return None;
    }
    let mut p = Peer { id: id.to_string(), name: String::new(), pc: String::new(), beat: 0, files: Vec::new(), collab: Vec::new() };
    for (k, v) in settings::parse(text) {
        match k {
            "name" => p.name = v.to_string(),
            "pc" => p.pc = v.to_string(),
            "beat" => p.beat = v.parse().unwrap_or(0),
            "collab" => p.collab.push(v.to_string()),
            "file" => {
                let mut f = v.splitn(3, '\t');
                let state = match f.next()? {
                    "edit" => FileState::Editing,
                    "lock" => FileState::Locked,
                    _ => FileState::Open,
                };
                let since = f.next()?.parse().unwrap_or(0);
                let path = f.next()?.to_string();
                p.files.push(PeerFile { key: file_key(&path), path, state, since });
            }
            _ => {}
        }
    }
    (!p.name.is_empty()).then_some(p)
}

enum Msg {
    Publish(String),
    Quit,
}

/// Running team connection: the worker thread and the latest snapshot.
pub struct Team {
    tx: mpsc::Sender<Msg>,
    wake: isize,
    thread: Option<std::thread::JoinHandle<()>>,
    pub dir: PathBuf,
    /// This Slate's presence id.
    pub me: String,
    pub name: String,
    pub pc: String,
    pub peers: Vec<Peer>,
    last: RefCell<String>,
    unc: RefCell<HashMap<PathBuf, String>>,
    /// When each own file entry started its current state.
    since: RefCell<HashMap<String, (FileState, u64)>>,
    pub reachable: bool,
}

impl Team {
    pub fn start(dir: PathBuf, hwnd: HWND) -> Team {
        let pc = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into());
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
        let me = format!("{}@{}-{}", sanitize(&user), sanitize(&pc), std::process::id());
        let (tx, rx) = mpsc::channel();
        let wake = unsafe { CreateEventW(None, false, false, None).map(|h| h.0 as isize).unwrap_or(0) };
        let (d, id, hw) = (dir.clone(), me.clone(), hwnd.0 as isize);
        let thread = std::thread::spawn(move || worker(d, id, rx, wake, hw));
        Team {
            tx,
            wake,
            thread: Some(thread),
            dir,
            me,
            name: my_name(),
            pc,
            peers: Vec::new(),
            last: RefCell::new(String::new()),
            unc: RefCell::new(HashMap::new()),
            since: RefCell::new(HashMap::new()),
            reachable: true,
        }
    }

    fn send(&self, m: Msg) {
        let _ = self.tx.send(m);
        if self.wake != 0 {
            unsafe {
                let _ = SetEvent(HANDLE(self.wake as *mut _));
            }
        }
    }

    pub fn shared(&self, p: &Path) -> String {
        if let Some(s) = self.unc.borrow().get(p) {
            return s.clone();
        }
        let s = shared_path(p);
        self.unc.borrow_mut().insert(p.to_path_buf(), s.clone());
        s
    }

    /// Others (not this Slate) that have the file open.
    pub fn others_on(&self, key: &str) -> Vec<(&Peer, &PeerFile)> {
        self.peers.iter().filter(|p| p.id != self.me).filter_map(|p| p.file(key).map(|f| (p, f))).collect()
    }

    /// Who holds the edit lock on a file: the earliest lock wins when two
    /// people locked at once. None for nobody; Some(None) for this Slate.
    pub fn lock_holder(&self, key: &str, mine: Option<u64>) -> Option<Option<&Peer>> {
        let mut best: Option<(u64, Option<&Peer>)> = mine.map(|s| (s, None));
        for p in self.peers.iter().filter(|p| p.id != self.me) {
            if let Some(f) = p.file(key).filter(|f| f.state == FileState::Locked) {
                if best.map_or(true, |(s, _)| (f.since, &p.id) < (s, &self.me)) {
                    best = Some((f.since, Some(p)));
                }
            }
        }
        best.map(|(_, p)| p)
    }
}

impl Drop for Team {
    fn drop(&mut self) {
        self.send(Msg::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if self.wake != 0 {
            unsafe {
                let _ = CloseHandle(HANDLE(self.wake as *mut _));
            }
        }
    }
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Watches the presence folder, keeps this Slate's file current and posts
/// every change to the window. Network trouble only delays updates.
fn worker(dir: PathBuf, id: String, rx: mpsc::Receiver<Msg>, wake: isize, hwnd: isize) {
    let folder = dir.join("presence");
    let mine = folder.join(format!("{id}.txt"));
    let mut content: Option<String> = None;
    let mut written = 0u64;
    let mut change = HANDLE::default();
    // (modified time, size) and parsed peer per presence file.
    let mut seen: HashMap<String, (SystemTime, u64, Option<Peer>)> = HashMap::new();
    let mut last_reachable = None;
    let mut last_ids: Vec<String> = Vec::new();
    loop {
        if change.is_invalid() || change.0.is_null() {
            let _ = std::fs::create_dir_all(&folder);
            let w: Vec<u16> = folder.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
            change = unsafe {
                FindFirstChangeNotificationW(PCWSTR(w.as_ptr()), false, FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE)
                    .unwrap_or_default()
            };
            written = 0;
        }
        // Own file: on change, and as a heartbeat.
        let t = now();
        let mut quit = false;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Publish(c) => {
                    content = Some(c);
                    written = 0;
                }
                Msg::Quit => quit = true,
            }
        }
        if quit {
            let _ = std::fs::remove_file(&mine);
            if !change.0.is_null() && !change.is_invalid() {
                unsafe {
                    let _ = FindCloseChangeNotification(change);
                }
            }
            return;
        }
        if let Some(c) = &content {
            if t >= written + BEAT_SECS {
                let tmp = folder.join(format!("{id}.tmp"));
                let body = c.replace("\nbeat=0\n", &format!("\nbeat={t}\n"));
                if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, &mine).is_ok() {
                    written = t;
                }
            }
        }
        // Everyone's files; only changed ones are read again.
        let mut peers = Vec::new();
        let mut reachable = false;
        let mut changed = false;
        if let Ok(rd) = std::fs::read_dir(&folder) {
            reachable = true;
            let mut present = Vec::new();
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let Some(stem) = name.strip_suffix(".txt") else { continue };
                let Ok(md) = e.metadata() else { continue };
                let (mt, len) = (md.modified().unwrap_or(UNIX_EPOCH), md.len());
                present.push(stem.to_string());
                let fresh = !matches!(seen.get(stem), Some((m, l, _)) if *m == mt && *l == len);
                if fresh {
                    let p = std::fs::read_to_string(e.path()).ok().and_then(|txt| parse(stem, &txt));
                    seen.insert(stem.to_string(), (mt, len, p));
                    changed = true;
                }
                if let Some((_, _, Some(p))) = seen.get(stem) {
                    let age = t.abs_diff(p.beat);
                    if age > PURGE_SECS && stem != id {
                        let _ = std::fs::remove_file(e.path());
                    } else if age <= STALE_SECS || stem == id {
                        peers.push(p.clone());
                    }
                }
            }
            let n = seen.len();
            seen.retain(|k, _| present.contains(k));
            changed |= seen.len() != n;
        }
        peers.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
        // Post only when a presence file changed, someone went stale, or the
        // folder came or went.
        let ids: Vec<String> = peers.iter().map(|p| p.id.clone()).collect();
        if changed || ids != last_ids || last_reachable != Some(reachable) {
            last_reachable = Some(reachable);
            last_ids = ids;
            let p = Box::into_raw(Box::new((reachable, peers)));
            unsafe {
                if PostMessageW(HWND(hwnd as *mut _), WM_TEAM, WPARAM(0), LPARAM(p as isize)).is_err() {
                    drop(Box::from_raw(p));
                }
            }
        }
        // Sleep until the folder changes, a publish arrives or the next beat.
        let handles: Vec<HANDLE> = [HANDLE(wake as *mut _), change].into_iter().filter(|h| !h.0.is_null() && !h.is_invalid()).collect();
        let timeout = if change.0.is_null() || change.is_invalid() { 15_000 } else { (BEAT_SECS * 1000 / 2) as u32 };
        let r = unsafe { WaitForMultipleObjects(&handles, false, timeout) };
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
    pub fn team_start(&mut self) {
        self.team = team_dir().map(|d| Team::start(d, self.hwnd));
        self.team_publish();
    }

    /// Sends this Slate's open files to the others when they changed.
    pub fn team_publish(&self) {
        let Some(t) = &self.team else { return };
        let mut files: Vec<PeerFile> = Vec::new();
        let mut since = t.since.borrow_mut();
        let mut keep = Vec::new();
        for b in &self.bufs {
            let Some(p) = &b.path else { continue };
            if b.loading.is_some() {
                continue;
            }
            let path = t.shared(p);
            let key = file_key(&path);
            let state = if b.lock_since.is_some() {
                FileState::Locked
            } else if b.dirty() {
                FileState::Editing
            } else {
                FileState::Open
            };
            let s = match since.get(&key) {
                Some(&(st, s)) if st == state => s,
                _ => b.lock_since.unwrap_or_else(now),
            };
            since.insert(key.clone(), (state, s));
            keep.push(key.clone());
            if !files.iter().any(|f| f.key == key) {
                files.push(PeerFile { key, path, state, since: s });
            }
        }
        since.retain(|k, _| keep.contains(k));
        drop(since);
        let collab = self.collab_ids();
        // "beat=0" is replaced with the time by the worker on each write.
        let text = render(&t.name, &t.pc, &files, &collab, 0);
        if *t.last.borrow() != text {
            *t.last.borrow_mut() = text.clone();
            t.send(Msg::Publish(text));
        }
    }

    pub fn on_team(&mut self, reachable: bool, peers: Vec<Peer>) {
        let Some(t) = self.team.as_mut() else { return };
        let was = t.reachable;
        t.reachable = reachable;
        let before: Vec<String> = t.peers.iter().filter(|p| p.id != t.me).map(|p| p.name.clone()).collect();
        t.peers = peers;
        if was && !reachable {
            self.flash("The team folder cannot be reached \u{2013} team features pause until it is back");
        }
        // Someone came online: a quiet note.
        if let Some(t) = &self.team {
            let joined: Vec<&Peer> = t.peers.iter().filter(|p| p.id != t.me && !before.contains(&p.name)).collect();
            if joined.len() == 1 && !before.is_empty() {
                let msg = format!("{} is online", joined[0].name);
                self.flash(&msg);
            }
        }
        self.team_apply_locks();
        self.collab_on_team();
        self.redraw();
    }

    /// Marks buffers locked by someone else read-only (and back).
    pub fn team_apply_locks(&mut self) {
        let Some(t) = &self.team else {
            for b in self.bufs.iter_mut() {
                b.locked_by = None;
            }
            return;
        };
        let mut changes = Vec::new();
        for (i, b) in self.bufs.iter().enumerate() {
            let holder = b.path.as_ref().and_then(|p| {
                let key = file_key(&t.shared(p));
                match t.lock_holder(&key, b.lock_since) {
                    Some(Some(peer)) => Some(peer.name.clone()),
                    _ => None,
                }
            });
            if holder != b.locked_by {
                changes.push((i, holder));
            }
        }
        for (i, holder) in changes {
            let name = self.bufs[i].name();
            match &holder {
                Some(h) => self.flash(&format!("{h} locked {name} for editing \u{2013} it is read-only here until they unlock it")),
                None => self.flash(&format!("{name} is unlocked")),
            }
            self.bufs[i].locked_by = holder;
        }
    }

    /// People (other than this Slate) with the buffer's file open.
    pub fn team_others(&self, bi: usize) -> Vec<(String, u32, FileState)> {
        let (Some(t), Some(p)) = (&self.team, &self.bufs[bi].path) else { return Vec::new() };
        let key = file_key(&t.shared(p));
        t.others_on(&key).into_iter().map(|(p, f)| (p.name.clone(), p.color(), f.state)).collect()
    }

    /// Note shown when a file opens that others are working on.
    pub fn team_on_open(&mut self, id: u32) {
        let bi = self.bi(id);
        let others = self.team_others(bi);
        if let Some(by) = &self.bufs[bi].locked_by {
            let msg = format!("Locked by {by} \u{2013} opened read-only");
            return self.flash(&msg);
        }
        let editing: Vec<&str> = others.iter().filter(|o| o.2 == FileState::Editing).map(|o| o.0.as_str()).collect();
        if !editing.is_empty() {
            let msg = format!("{} {} unsaved changes in this file \u{2013} Team: Co-edit avoids overwriting each other", join_names(&editing), if editing.len() == 1 { "has" } else { "have" });
            self.flash(&msg);
        } else if !others.is_empty() {
            let names: Vec<&str> = others.iter().map(|o| o.0.as_str()).collect();
            let msg = format!("{} also {} this file open", join_names(&names), if names.len() == 1 { "has" } else { "have" });
            self.flash(&msg);
        }
    }

    /// Warns once when you start changing a file someone else is changing.
    pub fn team_on_edit(&mut self) {
        let bi = self.bi(self.tab(self.av).buf);
        if self.bufs[bi].team_warned || !self.bufs[bi].dirty() || self.bufs[bi].collab.is_some() {
            return;
        }
        let others = self.team_others(bi);
        let editing: Vec<&str> = others.iter().filter(|o| o.2 == FileState::Editing).map(|o| o.0.as_str()).collect();
        if !editing.is_empty() {
            self.bufs[bi].team_warned = true;
            let msg = format!("Careful: {} also {} unsaved changes here \u{2013} the last save wins. Team: Co-edit works on it together", join_names(&editing), if editing.len() == 1 { "has" } else { "have" });
            self.flash(&msg);
        }
    }

    pub fn team_toggle_lock(&mut self) {
        if self.team.is_none() {
            return self.flash("Set a team folder first \u{2013} Team: Set Team Folder");
        }
        let bi = self.bi(self.tab(self.av).buf);
        if self.bufs[bi].path.is_none() {
            return self.flash("Save the file to the shared folder first");
        }
        if let Some(by) = &self.bufs[bi].locked_by {
            let msg = format!("{by} holds the lock on this file");
            return self.flash(&msg);
        }
        let on = self.bufs[bi].lock_since.is_none();
        self.bufs[bi].lock_since = on.then(now);
        self.team_publish();
        self.flash(if on { "Locked: others can open this file but not change it until you unlock it or close it" } else { "Unlocked" });
    }

    /// Who's online, and what they have open.
    pub fn team_people(&mut self) {
        let Some(t) = &self.team else {
            return self.flash("Team features are off \u{2013} Team: Set Team Folder turns them on");
        };
        let mut items = Vec::new();
        for p in t.peers.iter().filter(|p| p.id != t.me) {
            let summary = match p.files.len() {
                0 => "online".to_string(),
                1 => "1 file open".to_string(),
                n => format!("{n} files open"),
            };
            items.push(PalItem { label: p.name.clone(), hint: format!("{} \u{B7} {summary}", p.pc), act: PalAct::Cmd(crate::app::Cmd::TeamPeople) });
            for f in &p.files {
                let st = match f.state {
                    FileState::Open => "",
                    FileState::Editing => "editing \u{B7} ",
                    FileState::Locked => "locked \u{B7} ",
                };
                let name = Path::new(&f.path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let dir = Path::new(&f.path).parent().map(|d| d.display().to_string()).unwrap_or_default();
                items.push(PalItem { label: format!("      {name}"), hint: format!("{st}{dir}"), act: PalAct::Open(f.path.clone()) });
            }
        }
        let n = t.peers.iter().filter(|p| p.id != t.me).count();
        let title = if n == 0 { "Nobody else is online".to_string() } else { format!("{n} online \u{2013} pick a file to open it") };
        self.open_palette(PalMode::List, &title, items, "");
    }

    pub fn team_set_folder(&mut self, text: &str) {
        let text = text.trim().trim_matches('"').to_string();
        if !text.is_empty() && !Path::new(&text).is_dir() {
            return self.flash(&format!("Folder not found: {text}"));
        }
        settings::get_mut().team_dir = text.clone();
        settings::save();
        self.team = None;
        self.team_start();
        self.flash(if text.is_empty() { "Team features are off" } else { "Team folder set \u{2013} others using the same folder now see you online" });
        self.redraw();
    }

    pub fn team_set_name(&mut self, text: &str) {
        settings::get_mut().team_name = text.trim().to_string();
        settings::save();
        if let Some(t) = self.team.as_mut() {
            t.name = my_name();
            t.last.borrow_mut().clear();
        }
        self.team_publish();
        self.flash(&format!("Others see you as {}", my_name()));
    }

    /// Status-bar menu for the current file: co-edit, lock, who is here.
    pub fn team_file_menu(&mut self) {
        let bi = self.bi(self.tab(self.av).buf);
        let b = &self.bufs[bi];
        let mut items = Vec::new();
        let cmd = |c| PalAct::Cmd(c);
        use crate::app::Cmd;
        if b.collab.is_some() {
            items.push(PalItem { label: "Leave co-editing".into(), hint: "Ctrl+Shift+E".into(), act: cmd(Cmd::CoEdit) });
        } else if b.locked_by.is_none() {
            items.push(PalItem { label: "Co-edit this file with the others".into(), hint: "Ctrl+Shift+E".into(), act: cmd(Cmd::CoEdit) });
        }
        if b.locked_by.is_none() && b.collab.is_none() {
            let label = if b.lock_since.is_some() { "Unlock this file" } else { "Lock this file for editing" };
            items.push(PalItem { label: label.into(), hint: String::new(), act: cmd(Cmd::TeamLock) });
        }
        for (name, _, st) in self.team_others(bi) {
            let hint = match st {
                FileState::Open => "has it open",
                FileState::Editing => "unsaved changes",
                FileState::Locked => "locked it",
            };
            items.push(PalItem { label: name, hint: hint.into(), act: cmd(Cmd::TeamPeople) });
        }
        let title = self.bufs[bi].name();
        self.open_palette(PalMode::List, &title, items, "");
    }

    /// Status bar text for the current file: who else has it.
    pub fn team_status(&self) -> Option<String> {
        let bi = self.bi(self.tab(self.av).buf);
        let b = &self.bufs[bi];
        if let Some(by) = &b.locked_by {
            return Some(format!("\u{1F512} Locked by {by}"));
        }
        if b.lock_since.is_some() {
            return Some("\u{1F512} Locked by you".into());
        }
        let others = self.team_others(bi);
        match others.len() {
            0 => None,
            1 => Some(format!("{} {}", others[0].0, if others[0].2 == FileState::Editing { "editing" } else { "here" })),
            n => Some(format!("{n} others here")),
        }
    }

    /// Online people other than this Slate: (initials, colour, name).
    pub fn team_online(&self) -> Vec<(String, u32, String)> {
        let Some(t) = &self.team else { return Vec::new() };
        let mut seen: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for p in t.peers.iter().filter(|p| p.id != t.me) {
            // One avatar per person, even with several Slate windows; your
            // own other windows are not "someone online".
            if seen.contains(&p.name) || (p.name == t.name && p.pc == t.pc) {
                continue;
            }
            seen.push(p.name.clone());
            out.push((p.initials(), p.color(), p.name.clone()));
        }
        out
    }
}

pub fn join_names(n: &[&str]) -> String {
    match n.len() {
        0 => String::new(),
        1 => n[0].to_string(),
        2 => format!("{} and {}", n[0], n[1]),
        k => format!("{}, {} and {} others", n[0], n[1], k - 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_round_trip() {
        let files = vec![
            PeerFile { key: String::new(), path: r"\\srv\proj\P-101.pcf".into(), state: FileState::Editing, since: 5 },
            PeerFile { key: String::new(), path: r"\\srv\proj\list.csv".into(), state: FileState::Locked, since: 9 },
        ];
        let t = render("Ravi Kumar", "DDS-07", &files, &["abc".into()], 42);
        let p = parse("r@pc-1", &t).unwrap();
        assert_eq!((p.name.as_str(), p.pc.as_str(), p.beat), ("Ravi Kumar", "DDS-07", 42));
        assert_eq!(p.files.len(), 2);
        assert_eq!(p.files[0].key, r"\\srv\proj\p-101.pcf");
        assert_eq!(p.files[1].state, FileState::Locked);
        assert_eq!(p.collab, vec!["abc".to_string()]);
        assert!(parse("x", "garbage").is_none());
    }

    #[test]
    fn names() {
        assert_eq!(initials("Ravi Kumar"), "RK");
        assert_eq!(initials("vikram"), "Vi");
        assert_eq!(join_names(&["A", "B", "C", "D"]), "A, B and 2 others");
    }
}
