//! The team library: snippets, shared macros and find / replace presets.
//!
//! Every Slate keeps a full copy in `%APPDATA%\Slate\library` and the
//! copies sync over the LAN: when two Slates meet they swap lists of what
//! they hold (name, time changed, content hash, deleted or not) and fetch
//! whatever the other has newer; a change made here is sent to everyone
//! online at once. A deletion travels as a marker, so it is not undone by
//! the next PC that still has the old copy. Files edited by hand in the
//! folder are picked up the next time a library command runs.

use crate::app::{Action, App, PalAct, PalItem, PalMode};
use crate::buffer::EditKind;
use crate::edit;
use crate::wire::{LibMeta, Msg};
use std::collections::HashMap;
use std::path::PathBuf;

pub const SNIPPET: u8 = 0;
pub const MACRO: u8 = 1;
pub const PRESET: u8 = 2;
/// Deletion markers are forgotten after this long (days).
const TOMBSTONE_DAYS: u64 = 180;
const MAX_ITEM: usize = 4 << 20;

fn folder(kind: u8) -> (&'static str, &'static str) {
    match kind {
        SNIPPET => ("snippets", "txt"),
        MACRO => ("macros", "macro"),
        _ => ("presets", "txt"),
    }
}

pub fn lib_root() -> PathBuf {
    crate::settings::dir().join("library")
}

fn item_path(kind: u8, name: &str) -> PathBuf {
    let (dir, ext) = folder(kind);
    lib_root().join(dir).join(format!("{name}.{ext}"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn hash(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf29ce484222325u64, |h, &c| (h ^ c as u64).wrapping_mul(0x100000001b3))
}

/// A file name from what the user typed.
pub fn safe_name(s: &str) -> String {
    s.trim().chars().map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c }).collect::<String>().trim_matches('.').to_string()
}

/// Names from other PCs must be plain file names.
fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 120 && safe_name(s) == s
}

/// Whether `a` should replace `b`: newer, or the same moment and larger hash.
fn newer(a: &LibMeta, b: &LibMeta) -> bool {
    (a.mtime, a.hash, a.deleted) > (b.mtime, b.hash, b.deleted)
}

/// What this PC holds, kept in `library\index.txt`.
pub struct Store {
    pub items: HashMap<(u8, String), LibMeta>,
}

impl Store {
    pub fn load() -> Store {
        let mut items = HashMap::new();
        let t = std::fs::read_to_string(lib_root().join("index.txt")).unwrap_or_default();
        for l in t.lines() {
            let f: Vec<&str> = l.split('\t').collect();
            if let [k, m, h, d, name] = f.as_slice() {
                let (Ok(kind), Ok(mtime), Ok(hash)) = (k.parse::<u8>(), m.parse::<u64>(), u64::from_str_radix(h, 16)) else { continue };
                if kind <= PRESET && valid_name(name) {
                    items.insert((kind, name.to_string()), LibMeta { kind, name: name.to_string(), mtime, hash, deleted: *d == "1" });
                }
            }
        }
        Store { items }
    }

    pub fn save(&self) {
        let mut out = String::new();
        let mut v: Vec<&LibMeta> = self.items.values().collect();
        v.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
        for m in v {
            out += &format!("{}\t{}\t{:x}\t{}\t{}\n", m.kind, m.mtime, m.hash, m.deleted as u8, m.name);
        }
        let _ = std::fs::create_dir_all(lib_root());
        let _ = std::fs::write(lib_root().join("index.txt"), out);
    }

    /// Brings the index in line with the folders: new or edited files get
    /// the current time, removed ones a deletion marker. Returns what
    /// changed.
    pub fn scan(&mut self) -> Vec<LibMeta> {
        let mut changed = Vec::new();
        let now = now_ms();
        let mut present = Vec::new();
        for kind in [SNIPPET, MACRO, PRESET] {
            let (dir, ext) = folder(kind);
            let Ok(rd) = std::fs::read_dir(lib_root().join(dir)) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if !p.is_file() || !p.extension().map_or(false, |x| x.eq_ignore_ascii_case(ext)) {
                    continue;
                }
                let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                if !valid_name(&name) {
                    continue;
                }
                let Ok(body) = std::fs::read(&p) else { continue };
                let h = hash(&body);
                present.push((kind, name.clone()));
                let fresh = match self.items.get(&(kind, name.clone())) {
                    Some(m) => m.deleted || m.hash != h,
                    None => true,
                };
                if fresh {
                    let m = LibMeta { kind, name: name.clone(), mtime: now, hash: h, deleted: false };
                    self.items.insert((kind, name), m.clone());
                    changed.push(m);
                }
            }
        }
        for (k, m) in self.items.iter_mut() {
            if !m.deleted && !present.contains(k) {
                m.deleted = true;
                m.mtime = now;
                m.hash = 0;
                changed.push(m.clone());
            }
        }
        // Old deletion markers can go.
        let cutoff = now.saturating_sub(TOMBSTONE_DAYS * 86_400_000);
        let before = self.items.len();
        self.items.retain(|_, m| !(m.deleted && m.mtime < cutoff));
        if !changed.is_empty() || before != self.items.len() {
            self.save();
        }
        changed
    }

    /// Stores an item from another PC if it is newer. True when taken.
    pub fn take(&mut self, meta: LibMeta, body: &[u8]) -> bool {
        if meta.kind > PRESET || !valid_name(&meta.name) || body.len() > MAX_ITEM {
            return false;
        }
        let key = (meta.kind, meta.name.clone());
        if let Some(cur) = self.items.get(&key) {
            if !newer(&meta, cur) {
                return false;
            }
        }
        let p = item_path(meta.kind, &meta.name);
        if meta.deleted {
            let _ = std::fs::remove_file(&p);
        } else {
            if hash(body) != meta.hash {
                return false;
            }
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            if std::fs::write(&p, body).is_err() {
                return false;
            }
        }
        self.items.insert(key, meta);
        self.save();
        true
    }

    /// Items of `kind` that exist, sorted by name.
    pub fn list(&self, kind: u8) -> Vec<String> {
        let mut v: Vec<String> = self.items.values().filter(|m| m.kind == kind && !m.deleted).map(|m| m.name.clone()).collect();
        v.sort_by_key(|n| n.to_lowercase());
        v
    }
}

/// What `remote` lists that this store should fetch, and what it should
/// delete straight away.
pub fn wanted(store: &Store, remote: &[LibMeta]) -> (Vec<(u8, String)>, Vec<LibMeta>) {
    let mut want = Vec::new();
    let mut deletes = Vec::new();
    for m in remote {
        if m.kind > PRESET || !valid_name(&m.name) {
            continue;
        }
        let newer_there = store.items.get(&(m.kind, m.name.clone())).map_or(true, |cur| newer(m, cur));
        if newer_there {
            if m.deleted {
                deletes.push(m.clone());
            } else {
                want.push((m.kind, m.name.clone()));
            }
        }
    }
    (want, deletes)
}

pub fn encode_macro(m: &[Action]) -> String {
    let mut out = String::from("# Slate macro\r\n");
    for a in m {
        match a {
            Action::Char(c) => out += &format!("c {}\r\n", *c as u32),
            Action::Key(vk, ctrl, shift, alt) => out += &format!("k {vk} {} {} {}\r\n", *ctrl as u8, *shift as u8, *alt as u8),
        }
    }
    out
}

pub fn decode_macro(t: &str) -> Vec<Action> {
    t.lines()
        .filter_map(|l| {
            let f: Vec<u32> = l.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
            match (l.as_bytes().first(), f.as_slice()) {
                (Some(b'c'), [c]) => char::from_u32(*c).map(Action::Char),
                (Some(b'k'), [vk, c, s, a]) => Some(Action::Key(*vk, *c != 0, *s != 0, *a != 0)),
                _ => None,
            }
        })
        .collect()
}

impl App {
    /// Re-reads the library folders and tells the others about changes.
    pub fn lib_scan(&mut self) {
        let changed = self.library.scan();
        for m in changed {
            self.lib_push(&m);
        }
    }

    /// Sends one item (or its deletion) to everyone online.
    fn lib_push(&self, m: &LibMeta) {
        let Some(t) = &self.team else { return };
        let body = if m.deleted { Vec::new() } else { std::fs::read(item_path(m.kind, &m.name)).unwrap_or_default() };
        t.net.broadcast(&Msg::LibItem { meta: m.clone(), body });
    }

    pub fn lib_send_manifest(&self, to: u64) {
        let Some(t) = &self.team else { return };
        let items: Vec<LibMeta> = self.library.items.values().cloned().collect();
        t.net.send(to, &Msg::LibManifest { items });
    }

    pub fn lib_on_manifest(&mut self, from: u64, items: Vec<LibMeta>) {
        let (want, deletes) = wanted(&self.library, &items);
        for d in deletes {
            self.library.take(d, &[]);
        }
        if !want.is_empty() {
            if let Some(t) = &self.team {
                t.net.send(from, &Msg::LibWant { items: want });
            }
        }
    }

    pub fn lib_on_want(&mut self, from: u64, items: Vec<(u8, String)>) {
        let Some(t) = &self.team else { return };
        for (kind, name) in items.into_iter().take(10_000) {
            let Some(m) = self.library.items.get(&(kind, name.clone())) else { continue };
            if m.deleted {
                continue;
            }
            if let Ok(body) = std::fs::read(item_path(kind, &name)) {
                if hash(&body) == m.hash {
                    t.net.send(from, &Msg::LibItem { meta: m.clone(), body });
                }
            }
        }
    }

    pub fn lib_on_item(&mut self, meta: LibMeta, body: Vec<u8>) {
        self.library.take(meta, &body);
    }

    /// Saves an item here and sends it to everyone.
    fn lib_store(&mut self, kind: u8, name: &str, body: &[u8]) -> bool {
        let p = item_path(kind, name);
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        if std::fs::write(&p, body).is_err() {
            return false;
        }
        let m = LibMeta { kind, name: name.to_string(), mtime: now_ms(), hash: hash(body), deleted: false };
        self.library.items.insert((kind, name.to_string()), m.clone());
        self.library.save();
        self.lib_push(&m);
        true
    }

    fn lib_list(&mut self, kind: u8, act: fn(String) -> PalAct, title: &str, empty: &str) {
        self.lib_scan();
        let items: Vec<PalItem> = self
            .library
            .list(kind)
            .into_iter()
            .map(|name| {
                let hint = if kind == MACRO {
                    String::new()
                } else {
                    let t = std::fs::read_to_string(item_path(kind, &name)).unwrap_or_default();
                    if kind == PRESET {
                        let get = |k: &str| crate::settings::parse(&t).find(|(a, _)| *a == k).map(|(_, v)| v.to_string()).unwrap_or_default();
                        format!("{} \u{2192} {}", get("find"), get("replace"))
                    } else {
                        t.lines().next().map(|l| l.trim().chars().take(60).collect()).unwrap_or_default()
                    }
                };
                PalItem { label: name.clone(), hint, act: act(name) }
            })
            .collect();
        if items.is_empty() {
            return self.flash(empty);
        }
        self.open_palette(PalMode::List, title, items, "");
    }

    pub fn lib_snippets(&mut self) {
        self.lib_list(SNIPPET, PalAct::Snippet, "Insert snippet", "No snippets yet \u{2013} select text and use Library: Save Selection as Snippet");
    }

    pub fn lib_macros(&mut self) {
        self.lib_list(MACRO, PalAct::Macro, "Run shared macro", "No shared macros yet \u{2013} record one, then Library: Share Last Macro");
    }

    pub fn lib_presets(&mut self) {
        self.lib_list(PRESET, PalAct::Preset, "Find / replace preset", "No presets yet \u{2013} fill in Find and Replace, then Library: Save Find / Replace as Preset");
    }

    pub fn lib_insert(&mut self, name: &str) {
        let Ok(raw) = std::fs::read(item_path(SNIPPET, name)) else { return self.flash("Cannot read the snippet") };
        let mut text = crate::enc::decode(raw, None).text;
        // Stored with CRLF; the buffer keeps LF inside.
        text.retain(|&c| c != b'\r');
        let b = self.cur();
        edit::insert(b, &text, EditKind::Other);
        self.edited();
        self.reveal_caret();
        self.redraw();
    }

    pub fn lib_save_snippet(&mut self, name: &str) {
        let b = self.cur_ref();
        let text: Vec<u8> = b.sels.iter().filter(|s| !s.is_empty()).flat_map(|s| b.doc.text(s.lo(), s.hi())).collect();
        if text.is_empty() {
            return self.flash("Select the text to save first");
        }
        let name = safe_name(name);
        if name.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&text).replace("\r\n", "\n").replace('\n', "\r\n").into_bytes();
        if self.lib_store(SNIPPET, &name, &text) {
            self.flash(&format!("Snippet \u{201C}{name}\u{201D} saved and shared with the team"));
        } else {
            self.flash("Could not save the snippet");
        }
    }

    pub fn lib_run_macro(&mut self, name: &str) {
        let Ok(t) = std::fs::read_to_string(item_path(MACRO, name)) else { return self.flash("Cannot read the macro") };
        let m = decode_macro(&t);
        if m.is_empty() {
            return self.flash("The macro is empty");
        }
        self.last_macro = m;
        self.exec(crate::app::Cmd::MacroPlay);
    }

    pub fn lib_save_macro(&mut self, name: &str) {
        if self.last_macro.is_empty() {
            return self.flash("Record a macro first (Ctrl+Shift+R)");
        }
        let name = safe_name(name);
        if name.is_empty() {
            return;
        }
        let body = encode_macro(&self.last_macro);
        if self.lib_store(MACRO, &name, body.as_bytes()) {
            self.flash(&format!("Macro \u{201C}{name}\u{201D} shared with the team"));
        } else {
            self.flash("Could not save the macro");
        }
    }

    pub fn lib_use_preset(&mut self, name: &str) {
        let Ok(t) = std::fs::read_to_string(item_path(PRESET, name)) else { return self.flash("Cannot read the preset") };
        let get = |k: &str| crate::settings::parse(&t).find(|(a, _)| *a == k).map(|(_, v)| v.to_string()).unwrap_or_default();
        self.open_find(true, false);
        if let Some(fb) = self.find.as_mut() {
            fb.fields[0].set(&get("find"));
            fb.fields[1].set(&get("replace"));
            fb.case = get("case") == "1";
            fb.word = get("word") == "1";
            fb.regex = get("regex") == "1";
            fb.focus = 1;
        }
        self.recount();
        self.redraw();
    }

    pub fn lib_save_preset(&mut self, name: &str) {
        let Some(fb) = &self.find else { return self.flash("Fill in Find and Replace first (Ctrl+H)") };
        if fb.fields[0].text.is_empty() {
            return self.flash("Fill in Find and Replace first (Ctrl+H)");
        }
        let b = |v: bool| if v { "1" } else { "0" };
        let text = format!(
            "# Slate find / replace preset\r\nfind={}\r\nreplace={}\r\ncase={}\r\nword={}\r\nregex={}\r\n",
            fb.fields[0].text,
            fb.fields[1].text,
            b(fb.case),
            b(fb.word),
            b(fb.regex)
        );
        let name = safe_name(name);
        if name.is_empty() {
            return;
        }
        if self.lib_store(PRESET, &name, text.as_bytes()) {
            self.flash(&format!("Preset \u{201C}{name}\u{201D} saved and shared with the team"));
        } else {
            self.flash("Could not save the preset");
        }
    }

    /// Lists every item to delete one (for everyone).
    pub fn lib_delete_menu(&mut self) {
        self.lib_scan();
        let mut items = Vec::new();
        for (kind, what) in [(SNIPPET, "snippet"), (MACRO, "macro"), (PRESET, "preset")] {
            for name in self.library.list(kind) {
                items.push(PalItem { label: name.clone(), hint: what.into(), act: PalAct::LibDelete(kind, name) });
            }
        }
        if items.is_empty() {
            return self.flash("The library is empty");
        }
        self.open_palette(PalMode::List, "Delete from the team library", items, "");
    }

    pub fn lib_delete(&mut self, kind: u8, name: &str) {
        let _ = std::fs::remove_file(item_path(kind, name));
        let m = LibMeta { kind, name: name.to_string(), mtime: now_ms(), hash: 0, deleted: true };
        self.library.items.insert((kind, name.to_string()), m.clone());
        self.library.save();
        self.lib_push(&m);
        self.flash(&format!("\u{201C}{name}\u{201D} deleted for everyone"));
    }

    pub fn lib_open_folder(&mut self) {
        let dir = lib_root();
        for kind in [SNIPPET, MACRO, PRESET] {
            let _ = std::fs::create_dir_all(dir.join(folder(kind).0));
        }
        let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(kind: u8, name: &str, mtime: u64, hash: u64, deleted: bool) -> LibMeta {
        LibMeta { kind, name: name.into(), mtime, hash, deleted }
    }

    #[test]
    fn macro_round_trip() {
        let m = vec![Action::Char('é'), Action::Key(0x24, true, false, true), Action::Char('x')];
        let back = decode_macro(&encode_macro(&m));
        assert_eq!(format!("{back:?}"), format!("{m:?}"));
        assert_eq!(safe_name(" a/b:c "), "a_b_c");
        assert!(!valid_name("..\\evil"));
        assert!(!valid_name(".."));
        assert!(valid_name("Header block"));
    }

    #[test]
    fn newest_wins_and_deletions_travel() {
        let mut s = Store { items: HashMap::new() };
        s.items.insert((0, "a".into()), meta(0, "a", 10, 1, false));
        s.items.insert((0, "b".into()), meta(0, "b", 10, 1, false));
        let remote = vec![
            meta(0, "a", 20, 2, false), // newer there: fetch
            meta(0, "b", 5, 9, false),  // older there: keep ours
            meta(0, "c", 1, 3, false),  // new to us: fetch
            meta(1, "d", 4, 0, true),   // deleted, never had it: take the marker
            meta(0, "../x", 99, 1, false),
        ];
        let (want, del) = wanted(&s, &remote);
        assert_eq!(want, vec![(0, "a".to_string()), (0, "c".to_string())]);
        assert_eq!(del.len(), 1);
        // A deletion newer than our copy wins; an older one does not.
        assert!(newer(&meta(0, "a", 30, 0, true), &meta(0, "a", 20, 2, false)));
        assert!(!newer(&meta(0, "a", 15, 0, true), &meta(0, "a", 20, 2, false)));
    }
}
