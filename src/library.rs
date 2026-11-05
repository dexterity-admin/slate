//! The team library in `<team folder>\library`: snippets, shared macros and
//! find / replace presets, plain files anyone can add to or edit. It is
//! read when a command needs it, so nothing runs in the background.

use crate::app::{Action, App, PalAct, PalItem, PalMode};
use crate::edit;
use crate::buffer::EditKind;
use std::path::{Path, PathBuf};

fn lib_dir(app: &App, sub: &str) -> Option<PathBuf> {
    Some(app.team.as_ref()?.dir.join("library").join(sub))
}

/// Files of a library folder, sorted by name: (stem, path).
fn entries(dir: &Path, ext: &str) -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().map_or(false, |x| x.eq_ignore_ascii_case(ext)))
                .map(|p| (p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), p))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    v
}

/// A file name from what the user typed.
fn safe_name(s: &str) -> String {
    s.trim().chars().map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c }).collect()
}

pub fn encode_macro(m: &[Action]) -> String {
    let mut out = String::from("# Slate macro\n");
    for a in m {
        match a {
            Action::Char(c) => out += &format!("c {}\n", *c as u32),
            Action::Key(vk, ctrl, shift, alt) => out += &format!("k {vk} {} {} {}\n", *ctrl as u8, *shift as u8, *alt as u8),
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
    fn need_library(&mut self, sub: &str) -> Option<PathBuf> {
        let d = lib_dir(self, sub);
        if d.is_none() {
            self.flash("The library lives in the team folder \u{2013} Team: Set Team Folder");
        }
        d
    }

    pub fn lib_snippets(&mut self) {
        let Some(dir) = self.need_library("snippets") else { return };
        let items: Vec<PalItem> = entries(&dir, "txt")
            .into_iter()
            .map(|(name, p)| {
                let first = std::fs::read_to_string(&p).ok().and_then(|t| t.lines().next().map(|l| l.trim().chars().take(60).collect())).unwrap_or_default();
                PalItem { label: name, hint: first, act: PalAct::Snippet(p.display().to_string()) }
            })
            .collect();
        if items.is_empty() {
            return self.flash("No snippets yet \u{2013} select text and use Library: Save Selection as Snippet");
        }
        self.open_palette(PalMode::List, "Insert snippet", items, "");
    }

    pub fn lib_insert(&mut self, path: &str) {
        let Ok(raw) = std::fs::read(path) else { return self.flash("Cannot read the snippet") };
        let mut text = crate::enc::decode(raw, None).text;
        // Snippets are stored with CRLF; the buffer keeps LF inside.
        text.retain(|&c| c != b'\r');
        let b = self.cur();
        edit::insert(b, &text, EditKind::Other);
        self.after_lib_edit();
    }

    fn after_lib_edit(&mut self) {
        self.edited();
        self.reveal_caret();
        self.redraw();
    }

    pub fn lib_save_snippet(&mut self, name: &str) {
        let Some(dir) = self.need_library("snippets") else { return };
        let b = self.cur_ref();
        let text: Vec<u8> = b.sels.iter().filter(|s| !s.is_empty()).flat_map(|s| b.doc.text(s.lo(), s.hi())).collect();
        if text.is_empty() {
            return self.flash("Select the text to save first");
        }
        let name = safe_name(name);
        if name.is_empty() {
            return;
        }
        let text: Vec<u8> = String::from_utf8_lossy(&text).replace("\r\n", "\n").replace('\n', "\r\n").into_bytes();
        let _ = std::fs::create_dir_all(&dir);
        match std::fs::write(dir.join(format!("{name}.txt")), text) {
            Ok(()) => self.flash(&format!("Snippet \"{name}\" saved to the team library")),
            Err(e) => self.flash(&format!("Could not save the snippet: {e}")),
        }
    }

    pub fn lib_macros(&mut self) {
        let Some(dir) = self.need_library("macros") else { return };
        let items: Vec<PalItem> = entries(&dir, "macro").into_iter().map(|(name, p)| PalItem { label: name, hint: String::new(), act: PalAct::Macro(p.display().to_string()) }).collect();
        if items.is_empty() {
            return self.flash("No shared macros yet \u{2013} record one, then Library: Share Last Macro");
        }
        self.open_palette(PalMode::List, "Run shared macro", items, "");
    }

    pub fn lib_run_macro(&mut self, path: &str) {
        let Ok(t) = std::fs::read_to_string(path) else { return self.flash("Cannot read the macro") };
        let m = decode_macro(&t);
        if m.is_empty() {
            return self.flash("The macro is empty");
        }
        self.last_macro = m;
        self.exec(crate::app::Cmd::MacroPlay);
    }

    pub fn lib_save_macro(&mut self, name: &str) {
        let Some(dir) = self.need_library("macros") else { return };
        if self.last_macro.is_empty() {
            return self.flash("Record a macro first (Ctrl+Shift+R)");
        }
        let name = safe_name(name);
        if name.is_empty() {
            return;
        }
        let _ = std::fs::create_dir_all(&dir);
        match std::fs::write(dir.join(format!("{name}.macro")), encode_macro(&self.last_macro)) {
            Ok(()) => self.flash(&format!("Macro \"{name}\" shared in the team library")),
            Err(e) => self.flash(&format!("Could not save the macro: {e}")),
        }
    }

    pub fn lib_presets(&mut self) {
        let Some(dir) = self.need_library("presets") else { return };
        let items: Vec<PalItem> = entries(&dir, "txt")
            .into_iter()
            .map(|(name, p)| {
                let t = std::fs::read_to_string(&p).unwrap_or_default();
                let get = |k: &str| crate::settings::parse(&t).find(|(a, _)| *a == k).map(|(_, v)| v.to_string()).unwrap_or_default();
                PalItem { label: name, hint: format!("{} \u{2192} {}", get("find"), get("replace")), act: PalAct::Preset(p.display().to_string()) }
            })
            .collect();
        if items.is_empty() {
            return self.flash("No presets yet \u{2013} fill in Find and Replace, then Library: Save Find / Replace as Preset");
        }
        self.open_palette(PalMode::List, "Find / replace preset", items, "");
    }

    pub fn lib_use_preset(&mut self, path: &str) {
        let Ok(t) = std::fs::read_to_string(path) else { return self.flash("Cannot read the preset") };
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
        let Some(dir) = self.need_library("presets") else { return };
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
        let _ = std::fs::create_dir_all(&dir);
        match std::fs::write(dir.join(format!("{name}.txt")), text) {
            Ok(()) => self.flash(&format!("Preset \"{name}\" saved to the team library")),
            Err(e) => self.flash(&format!("Could not save the preset: {e}")),
        }
    }

    pub fn lib_open_folder(&mut self) {
        let Some(dir) = self.need_library("") else { return };
        for sub in ["snippets", "macros", "presets"] {
            let _ = std::fs::create_dir_all(dir.join(sub));
        }
        let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
    }

    /// Instant search through the team index, on a worker thread; results
    /// open like Find in Files.
    pub fn team_search(&mut self, text: &str) {
        let Some(team) = self.team.as_ref().map(|t| t.dir.clone()) else {
            return self.flash("Instant search uses the team folder \u{2013} Team: Set Team Folder");
        };
        let pat = text.trim().to_string();
        if pat.chars().count() < 3 {
            return self.flash("Type at least 3 characters");
        }
        let q = crate::search::Query { pat, case: false, word: false, regex: false };
        self.last_find = q.clone();
        self.flash(&format!("Searching the team index for \u{201C}{}\u{201D}\u{2026}", q.pat));
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let out = crate::index::search(&team, &q);
            let p = Box::into_raw(Box::new(out));
            unsafe {
                use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(HWND(hwnd as *mut _), crate::app::WM_FIF_DONE, WPARAM(0), LPARAM(p as isize));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macro_round_trip() {
        let m = vec![Action::Char('é'), Action::Key(0x24, true, false, true), Action::Char('x')];
        let back = decode_macro(&encode_macro(&m));
        assert_eq!(format!("{back:?}"), format!("{m:?}"));
        assert_eq!(safe_name(" a/b:c "), "a_b_c");
    }
}
