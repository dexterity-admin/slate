//! Team features over direct LAN connections, with nothing to set up:
//! presence (who is online and which files they have open), open-file
//! warnings and edit locks. The connections themselves are in net.rs;
//! co-editing, the library and search build on the same peers.

use crate::app::{App, PalAct, PalItem, PalMode};
use crate::net::{Event, Net, Sink};
use crate::settings;
use crate::wire::{Msg, PresenceFile, SessionInfo};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use windows::core::PCWSTR;

/// Colours people are drawn in, picked from their name.
pub const COLORS: [u32; 8] = [0x58A6FF, 0x3FB950, 0xD29922, 0xF778BA, 0xA371F7, 0x39C5CF, 0xF0883E, 0xE5534B];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileState {
    Open,
    /// Has unsaved changes.
    Editing,
    Locked,
}

impl FileState {
    fn code(self) -> u8 {
        match self {
            FileState::Open => 0,
            FileState::Editing => 1,
            FileState::Locked => 2,
        }
    }
    fn from_code(c: u8) -> FileState {
        match c {
            1 => FileState::Editing,
            2 => FileState::Locked,
            _ => FileState::Open,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PeerFile {
    /// Comparison key, see `file_key`.
    pub key: String,
    pub path: String,
    pub state: FileState,
    pub since: u64,
}

#[derive(Clone, Debug)]
pub struct Peer {
    pub id: u64,
    pub name: String,
    pub pc: String,
    pub version: String,
    /// A search hub, not a person.
    pub hub: bool,
    pub files: Vec<PeerFile>,
    /// Co-editing sessions this peer is in.
    pub sessions: Vec<SessionInfo>,
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
    let words: Vec<&str> = name.split_whitespace().collect();
    match words.as_slice() {
        [] => "?".into(),
        [one] => {
            let mut c = one.chars();
            let first = c.next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            first + &c.next().map(|c| c.to_lowercase().to_string()).unwrap_or_default()
        }
        [a, .., b] => {
            let f = |w: &str| w.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            f(a) + &f(b)
        }
    }
}

/// The name shown to others: the optional setting, else the full name
/// Windows knows (on a domain), else the user name.
pub fn my_name() -> String {
    if let Ok(n) = std::env::var("SLATE_TEAM_NAME") {
        return n;
    }
    let s = settings::get().team_name.clone();
    if !s.trim().is_empty() {
        return s.trim().to_string();
    }
    use windows::Win32::Security::Authentication::Identity::{GetUserNameExW, NameDisplay};
    let mut buf = [0u16; 256];
    let mut n = buf.len() as u32;
    if unsafe { GetUserNameExW(NameDisplay, windows::core::PWSTR(buf.as_mut_ptr()), &mut n) }.as_bool() && n > 0 {
        let full = String::from_utf16_lossy(&buf[..n as usize]);
        if !full.trim().is_empty() {
            return full.trim().to_string();
        }
    }
    let u = std::env::var("USERNAME").unwrap_or_else(|_| "Someone".into());
    let mut c = u.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => u,
    }
}

pub fn my_pc() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into())
}

/// Comparison key for a file: a `\\server\share` path names the same file
/// on every PC; any other path only on the PC that has it.
pub fn file_key(path: &str, pc: &str) -> String {
    let p = path.replace('/', "\\").to_lowercase();
    if p.starts_with("\\\\") {
        p
    } else {
        format!("{}|{p}", pc.to_lowercase())
    }
}

/// The path as other PCs see it: a mapped drive letter becomes its UNC
/// path. Local paths come back unchanged.
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

pub struct Team {
    pub net: Net,
    pub me: u64,
    pub name: String,
    pub pc: String,
    pub peers: HashMap<u64, Peer>,
    started: Instant,
    last: RefCell<Option<Msg>>,
    unc: RefCell<HashMap<PathBuf, String>>,
    /// When each own file entry started its current state.
    since: RefCell<HashMap<String, (FileState, u64)>>,
}

impl Team {
    pub fn start(hwnd: windows::Win32::Foundation::HWND) -> Option<Team> {
        let me = crate::net::new_id();
        let (name, pc) = (my_name(), my_pc());
        let net = Net::start(me, name.clone(), pc.clone(), false, Sink::Window(hwnd.0 as isize)).ok()?;
        Some(Team {
            net,
            me,
            name,
            pc,
            peers: HashMap::new(),
            started: Instant::now(),
            last: RefCell::new(None),
            unc: RefCell::new(HashMap::new()),
            since: RefCell::new(HashMap::new()),
        })
    }

    pub fn shared(&self, p: &Path) -> String {
        if let Some(s) = self.unc.borrow().get(p) {
            return s.clone();
        }
        let s = shared_path(p);
        self.unc.borrow_mut().insert(p.to_path_buf(), s.clone());
        s
    }

    pub fn key_of(&self, p: &Path) -> String {
        file_key(&self.shared(p), &self.pc)
    }

    /// People (not hubs) sorted by name.
    pub fn people(&self) -> Vec<&Peer> {
        let mut v: Vec<&Peer> = self.peers.values().filter(|p| !p.hub).collect();
        v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
        v
    }

    /// Others that have the file open.
    pub fn others_on(&self, key: &str) -> Vec<(&Peer, &PeerFile)> {
        self.people().into_iter().filter_map(|p| p.file(key).map(|f| (p, f))).collect()
    }

    /// Who holds the edit lock on a file; the earliest lock wins when two
    /// people locked at once. None for nobody; Some(None) for this Slate.
    pub fn lock_holder(&self, key: &str, mine: Option<u64>) -> Option<Option<&Peer>> {
        let mut best: Option<(u64, u64, Option<&Peer>)> = mine.map(|s| (s, self.me, None));
        for p in self.people() {
            if let Some(f) = p.file(key).filter(|f| f.state == FileState::Locked) {
                if best.map_or(true, |(s, id, _)| (f.since, p.id) < (s, id)) {
                    best = Some((f.since, p.id, Some(p)));
                }
            }
        }
        best.map(|(_, _, p)| p)
    }

    pub fn hubs(&self) -> Vec<&Peer> {
        let mut v: Vec<&Peer> = self.peers.values().filter(|p| p.hub).collect();
        v.sort_by_key(|p| p.id);
        v
    }
}

impl App {
    pub fn team_start(&mut self) {
        if !settings::get().team {
            self.team = None;
            return;
        }
        self.team = Team::start(self.hwnd);
        self.lib_scan();
        self.team_publish();
    }

    /// The presence message for this Slate's open files and sessions.
    fn presence(&self) -> Option<Msg> {
        let t = self.team.as_ref()?;
        let mut files: Vec<PresenceFile> = Vec::new();
        let mut since = t.since.borrow_mut();
        let mut keep = Vec::new();
        for b in &self.bufs {
            let Some(p) = &b.path else { continue };
            if b.loading.is_some() {
                continue;
            }
            let path = t.shared(p);
            let key = file_key(&path, &t.pc);
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
            if !keep.contains(&key) {
                keep.push(key);
                files.push(PresenceFile { path, state: state.code(), since: s });
            }
        }
        since.retain(|k, _| keep.contains(k));
        drop(since);
        Some(Msg::Presence { files, sessions: self.collab_sessions() })
    }

    /// Tells the others when this Slate's files or sessions changed.
    pub fn team_publish(&self) {
        let Some(m) = self.presence() else { return };
        let t = self.team.as_ref().unwrap();
        if t.last.borrow().as_ref() != Some(&m) {
            t.net.broadcast(&m);
            *t.last.borrow_mut() = Some(m);
        }
    }

    pub fn on_net(&mut self, e: Event) {
        if self.team.is_none() {
            return;
        }
        match e {
            Event::Up { id, name, pc, version, hub } => {
                let t = self.team.as_mut().unwrap();
                let quiet = t.started.elapsed().as_secs() < 8 || hub || name == t.name;
                t.peers.insert(id, Peer { id, name: name.clone(), pc, version, hub, files: Vec::new(), sessions: Vec::new() });
                // Bring the newcomer up to date.
                if let Some(m) = t.last.borrow().clone() {
                    t.net.send(id, &m);
                }
                self.lib_send_manifest(id);
                self.collab_peer_up(id);
                if !quiet {
                    self.flash(&format!("{name} is online"));
                }
            }
            Event::Down { id } => {
                self.team.as_mut().unwrap().peers.remove(&id);
                self.collab_peer_down(id);
            }
            Event::Msg { from, msg } => {
                let t = self.team.as_mut().unwrap();
                let Some(p) = t.peers.get_mut(&from) else { return };
                match msg {
                    Msg::Presence { files, sessions } => {
                        let pc = p.pc.clone();
                        p.files = files.into_iter().map(|f| PeerFile { key: file_key(&f.path, &pc), path: f.path, state: FileState::from_code(f.state), since: f.since }).collect();
                        p.sessions = sessions;
                        self.collab_on_presence(from);
                    }
                    Msg::LibManifest { items } => self.lib_on_manifest(from, items),
                    Msg::LibWant { items } => self.lib_on_want(from, items),
                    Msg::LibItem { meta, body } => self.lib_on_item(meta, body),
                    Msg::SearchRes { qid, text } => self.search_result(qid, text),
                    Msg::SearchReq { .. } | Msg::Hello { .. } | Msg::Bye => {}
                    m => self.collab_on_msg(from, m),
                }
            }
        }
        self.team_apply_locks();
        self.redraw();
    }

    /// Instant search: asks the search hub on the network.
    pub fn team_search(&mut self, text: &str) {
        let pat = text.trim().to_string();
        if pat.chars().count() < 3 {
            return self.flash("Type at least 3 characters");
        }
        let Some(t) = &self.team else {
            return self.flash("Team features are off \u{2013} Settings: Toggle Team Features");
        };
        let Some(hub) = t.hubs().first().map(|h| h.id) else {
            return self.flash("No search hub is running on the network \u{2013} Find in Files (Ctrl+Shift+F) searches a folder directly");
        };
        let qid = crate::net::new_id();
        t.net.send(hub, &Msg::SearchReq { qid, query: pat.clone() });
        self.search_qid = Some(qid);
        self.last_find = crate::search::Query { pat: pat.clone(), case: false, word: false, regex: false };
        self.flash(&format!("Searching for \u{201C}{pat}\u{201D}\u{2026}"));
    }

    pub fn search_result(&mut self, qid: u64, text: String) {
        if self.search_qid == Some(qid) {
            self.search_qid = None;
            self.fif_done(text);
        }
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
            let holder = b.path.as_ref().and_then(|p| match t.lock_holder(&t.key_of(p), b.lock_since) {
                Some(Some(peer)) => Some(peer.name.clone()),
                _ => None,
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

    /// People with the buffer's file open: (name, colour, state).
    pub fn team_others(&self, bi: usize) -> Vec<(String, u32, FileState)> {
        let (Some(t), Some(p)) = (&self.team, &self.bufs[bi].path) else { return Vec::new() };
        t.others_on(&t.key_of(p)).into_iter().map(|(p, f)| (p.name.clone(), p.color(), f.state)).collect()
    }

    /// Note shown when a file opens that others are working on.
    pub fn team_on_open(&mut self, id: u32) {
        let bi = self.bi(id);
        if let Some(by) = self.bufs[bi].locked_by.clone() {
            return self.flash(&format!("Locked by {by} \u{2013} opened read-only"));
        }
        if let Some(who) = self.collab_offer_for(bi) {
            return self.flash(&format!("{who} co-editing this file \u{2013} Ctrl+Shift+E joins"));
        }
        let others = self.team_others(bi);
        let editing: Vec<&str> = others.iter().filter(|o| o.2 == FileState::Editing).map(|o| o.0.as_str()).collect();
        if !editing.is_empty() {
            let msg = format!("{} {} unsaved changes in this file \u{2013} Ctrl+Shift+E co-edits it instead", join_names(&editing), if editing.len() == 1 { "has" } else { "have" });
            self.flash(&msg);
        } else if !others.is_empty() {
            let names: Vec<&str> = others.iter().map(|o| o.0.as_str()).collect();
            let msg = format!("{} also {} this file open", join_names(&names), if names.len() == 1 { "has" } else { "have" });
            self.flash(&msg);
        }
    }

    /// Warns once when you start changing a file someone else is changing.
    pub fn team_on_edit(&mut self) {
        if self.views[self.av].tabs.is_empty() {
            return;
        }
        let bi = self.bi(self.tab(self.av).buf);
        if self.bufs[bi].team_warned || !self.bufs[bi].dirty() || self.bufs[bi].collab.is_some() {
            return;
        }
        let others = self.team_others(bi);
        let editing: Vec<&str> = others.iter().filter(|o| o.2 == FileState::Editing).map(|o| o.0.as_str()).collect();
        if !editing.is_empty() {
            self.bufs[bi].team_warned = true;
            let msg = format!("Careful: {} also {} unsaved changes here \u{2013} the last save wins. Ctrl+Shift+E co-edits it together", join_names(&editing), if editing.len() == 1 { "has" } else { "have" });
            self.flash(&msg);
        }
    }

    pub fn team_toggle_lock(&mut self) {
        if self.team.is_none() {
            return self.flash("Team features are off \u{2013} Settings: Toggle Team Features");
        }
        let bi = self.bi(self.tab(self.av).buf);
        if self.bufs[bi].path.is_none() {
            return self.flash("Save the file first");
        }
        if self.bufs[bi].collab.is_some() {
            return self.flash("A co-edited file cannot be locked");
        }
        if let Some(by) = self.bufs[bi].locked_by.clone() {
            return self.flash(&format!("{by} holds the lock on this file"));
        }
        let on = self.bufs[bi].lock_since.is_none();
        self.bufs[bi].lock_since = on.then(now);
        self.team_publish();
        self.flash(if on { "Locked: others can open this file but not change it until you unlock it or close it" } else { "Unlocked" });
    }

    /// Who's online, and what they have open.
    pub fn team_people(&mut self) {
        let Some(t) = &self.team else {
            return self.flash("Team features are off \u{2013} Settings: Toggle Team Features turns them on");
        };
        let mut items = Vec::new();
        for p in t.people() {
            let summary = match p.files.len() {
                0 => "online".to_string(),
                1 => "1 file open".to_string(),
                n => format!("{n} files open"),
            };
            // Say so when someone runs another version of Slate.
            let ver = if p.version != env!("CARGO_PKG_VERSION") { format!(" \u{B7} Slate {}", p.version) } else { String::new() };
            items.push(PalItem { label: p.name.clone(), hint: format!("{} \u{B7} {summary}{ver}", p.pc), act: PalAct::Cmd(crate::app::Cmd::TeamPeople) });
            for f in &p.files {
                let st = match f.state {
                    FileState::Open => "",
                    FileState::Editing => "editing \u{B7} ",
                    FileState::Locked => "locked \u{B7} ",
                };
                let name = Path::new(&f.path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let dir = Path::new(&f.path).parent().map(|d| d.display().to_string()).unwrap_or_default();
                // Another PC's local file cannot be opened from here.
                let act = if f.path.starts_with("\\\\") || p.pc.eq_ignore_ascii_case(&t.pc) { PalAct::Open(f.path.clone()) } else { PalAct::Cmd(crate::app::Cmd::TeamPeople) };
                items.push(PalItem { label: format!("      {name}"), hint: format!("{st}{dir}"), act });
            }
        }
        for h in t.hubs() {
            items.push(PalItem { label: "Search hub".into(), hint: h.pc.clone(), act: PalAct::Cmd(crate::app::Cmd::TeamSearch) });
        }
        let n = t.people().len();
        let title = if n == 0 { "Nobody else is online".to_string() } else { format!("{n} online \u{2013} pick a file to open it") };
        self.open_palette(PalMode::List, &title, items, "");
    }

    pub fn team_set_name(&mut self, text: &str) {
        settings::get_mut().team_name = text.trim().to_string();
        settings::save();
        self.flash(&format!("Others see you as {} after Slate restarts", my_name()));
    }

    pub fn team_toggle(&mut self) {
        let on = {
            let mut s = settings::get_mut();
            s.team = !s.team;
            s.team
        };
        settings::save();
        if on {
            self.team_start();
        } else {
            for b in self.bufs.iter_mut() {
                b.collab = None;
                b.locked_by = None;
                b.lock_since = None;
            }
            self.team = None;
        }
        self.flash(if on { "Team features on" } else { "Team features off: Slate no longer talks to other PCs" });
        self.redraw();
    }

    /// Status-bar menu for the current file: co-edit, lock, who is here.
    pub fn team_file_menu(&mut self) {
        let bi = self.bi(self.tab(self.av).buf);
        let b = &self.bufs[bi];
        let mut items = Vec::new();
        use crate::app::Cmd;
        let cmd = PalAct::Cmd;
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
        for p in t.people() {
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
    fn names_and_keys() {
        assert_eq!(initials("Ravi Kumar"), "RK");
        assert_eq!(initials("Ravi Kumar Shah"), "RS");
        assert_eq!(initials("vikram"), "Vi");
        assert_eq!(join_names(&["A", "B", "C", "D"]), "A, B and 2 others");
        assert_eq!(file_key(r"\\Srv\P\A.pcf", "PC1"), r"\\srv\p\a.pcf");
        assert_eq!(file_key(r"D:\x\A.pcf", "PC1"), r"pc1|d:\x\a.pcf");
        assert_ne!(file_key(r"D:\x\A.pcf", "PC1"), file_key(r"D:\x\A.pcf", "PC2"));
    }
}
