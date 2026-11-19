//! Updater glue: background checks, the status-bar notice, install and the
//! hand-over to the new process (with roll-back if it never reports in).

use crate::app::App;
use crate::settings;
use crate::dialog::{Mark, Spec};
use crate::sys;
use crate::update::{self, Manifest, UpdateMsg};
use std::path::PathBuf;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, KillTimer, PostMessageW, SetTimer, ShowWindow, SW_HIDE, SW_SHOW, WM_APP,
};

pub const WM_UPDATE: u32 = WM_APP + 3;
pub const WM_UPDATE_OK: u32 = WM_APP + 4;
pub const T_UPDATE: usize = 5;
pub const T_HANDSHAKE: usize = 6;
pub const T_CLEANUP: usize = 7;
const RECHECK_MS: u32 = 6 * 60 * 60 * 1000;

#[derive(Default)]
pub struct UpdateState {
    pub available: Option<Manifest>,
    pub busy: bool,
    /// Child started after an install, and when.
    pub child: Option<(std::process::Child, Instant, String)>,
    /// Set in the new process: the old window to notify and its version.
    pub handover: Option<(isize, String)>,
}

impl App {
    pub fn updates_enabled(&self) -> bool {
        !self.no_session
    }

    /// Starts the periodic check (first one shortly after start-up).
    pub fn schedule_update_checks(&self) {
        if self.updates_enabled() && settings::get().auto_update {
            unsafe {
                SetTimer(self.hwnd, T_UPDATE, 20_000, None);
            }
        }
    }

    pub fn start_check(&mut self, manual: bool) {
        if self.upd.busy {
            return;
        }
        if manual {
            self.flash("Checking for updates…");
        }
        self.upd.busy = true;
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let msg = Box::new(update::check());
            post(hwnd, manual, msg);
        });
    }

    pub fn on_update_timer(&mut self) {
        unsafe {
            SetTimer(self.hwnd, T_UPDATE, RECHECK_MS, None);
        }
        self.start_check(false);
    }

    pub fn on_update_msg(&mut self, msg: UpdateMsg, manual: bool) {
        self.upd.busy = false;
        match msg {
            UpdateMsg::Available(m) => {
                let skipped = settings::get().skip_version == m.version;
                if !skipped || manual {
                    self.flash(&format!("Slate {} is available – click \"Update\" in the status bar", m.version));
                    self.upd.available = Some(m);
                    if manual {
                        self.install_update();
                    }
                }
            }
            UpdateMsg::UpToDate => {
                self.upd.available = None;
                if manual {
                    self.flash(&format!("Slate {} is up to date", update::current_version()));
                    self.redraw();
                    let spec = Spec::new(Mark::Logo, "Slate is up to date", &format!("You have the latest version, {}.", update::current_version()));
                    crate::dialog::show(self.hwnd, spec);
                }
            }
            UpdateMsg::Failed(e) => {
                // Background checks fail quietly (offline, no release yet).
                if manual {
                    self.flash("Update check failed");
                    self.redraw();
                    sys::error(self.hwnd, "Couldn\u{2019}t check for updates", &format!("{e}\n\nCheck the network connection and try again, or get the latest version from the website."));
                }
            }
            UpdateMsg::Ready(m, path) => self.apply_update(m, path),
        }
        self.redraw();
    }

    /// Slate's About page: version, update status, where settings live.
    pub fn about(&mut self) {
        let ver = update::current_version();
        let (status, button) = match (&self.upd.available, self.upd.busy) {
            (Some(m), _) => (format!("Slate {} is available", m.version), format!("Install {}", m.version)),
            (None, true) => ("Checking\u{2026}".to_string(), "Check for updates".to_string()),
            (None, false) if !self.updates_enabled() => ("Off in this window".to_string(), "Check for updates".to_string()),
            (None, false) => (if settings::get().auto_update { "Checked automatically every 6 hours".to_string() } else { "Automatic checks are off".to_string() }, "Check for updates".to_string()),
        };
        let dir = settings::dir().display().to_string();
        let site = "https://dexterity-admin.github.io/slate/";
        let releases = format!("https://github.com/{}/releases", update::REPO);
        let spec = Spec {
            mark: Mark::Logo,
            title: "Slate".into(),
            sub: "A small, fast text editor for engineering work.".into(),
            body: String::new(),
            rows: vec![
                ("Version".into(), ver.to_string(), None),
                ("Updates".into(), status, None),
                ("Settings folder".into(), dir.clone(), Some(dir)),
                ("Website".into(), "dexterity-admin.github.io/slate".into(), Some(site.into())),
                ("What\u{2019}s new".into(), "Release notes on GitHub".into(), Some(releases)),
                ("Every command".into(), "Ctrl+Shift+P or F1".into(), None),
            ],
            buttons: vec![button, "Close".into()],
            primary: 1,
            cancel: 1,
            width: 500.0,
        };
        if crate::dialog::show(self.hwnd, spec) == 0 {
            if self.upd.available.is_some() {
                self.install_update();
            } else {
                self.start_check(true);
            }
        }
    }

    /// Asks, then downloads and installs the available update.
    pub fn install_update(&mut self) {
        let Some(m) = self.upd.available.clone() else {
            self.start_check(true);
            return;
        };
        // The notes' first line is their headline ("Slate 1.5.3: what it is").
        let mut lines = m.notes.lines();
        let first = lines.next().unwrap_or("").trim();
        let headline = first.split_once(':').map(|(_, h)| h.trim()).filter(|h| !h.is_empty()).unwrap_or(first);
        let mut cs = headline.chars();
        let headline: String = cs.next().map(|c| c.to_uppercase().chain(cs).collect()).unwrap_or_default();
        let rest: String = lines.collect::<Vec<_>>().join("\n").trim().to_string();
        let spec = Spec {
            mark: Mark::Logo,
            title: format!("Slate {} is ready to install", m.version),
            sub: if headline.is_empty() { format!("You have {}.", update::current_version()) } else { format!("{headline} \u{2013} you have {}.", update::current_version()) },
            body: format!("{rest}{}Slate restarts and reopens all your tabs, unsaved text included.", if rest.is_empty() { "" } else { "\n\n" }),
            rows: Vec::new(),
            buttons: vec!["Install and restart".into(), "Later".into(), "Skip this version".into()],
            primary: 0,
            cancel: 1,
            width: 520.0,
        };
        match crate::dialog::show(self.hwnd, spec) {
            0 => {}
            1 => return,
            _ => {
                settings::get_mut().skip_version = m.version.clone();
                settings::save();
                self.upd.available = None;
                self.redraw();
                return;
            }
        }
        self.upd.busy = true;
        self.flash(&format!("Downloading Slate {}…", m.version));
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let msg = Box::new(update::download(&m));
            post(hwnd, true, msg);
        });
    }

    /// Test hook (tools\rollback-test.ps1): installs a local file as if it
    /// were a downloaded update, through the real swap, start and
    /// handshake. Only runs when SLATE_TEST_UPDATE names the file.
    pub fn test_update(&mut self) {
        let Some(p) = std::env::var_os("SLATE_TEST_UPDATE") else { return };
        // The new process must not install it again.
        std::env::remove_var("SLATE_TEST_UPDATE");
        let m = Manifest { version: "9.9.9".into(), size: 0, sha256: String::new(), notes: String::new() };
        self.apply_update(m, PathBuf::from(p));
    }

    fn apply_update(&mut self, m: Manifest, path: PathBuf) {
        // Everything open, including unsaved text, goes into the session first.
        self.save_session_forced();
        let exe = match update::swap_in(&path) {
            Ok(e) => e,
            Err(e) => {
                self.flash(&format!("Update not installed: {e}"));
                return;
            }
        };
        let child = std::process::Command::new(&exe)
            .arg("-multiInst")
            .arg("--after-update")
            .arg((self.hwnd.0 as isize).to_string())
            .arg(update::current_version())
            .spawn();
        match child {
            Ok(c) => {
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                    SetTimer(self.hwnd, T_HANDSHAKE, 500, None);
                }
                self.upd.child = Some((c, Instant::now(), m.version));
            }
            Err(e) => {
                update::roll_back();
                self.flash(&format!("Update not installed: could not start the new version ({e})"));
            }
        }
    }

    /// Polls the new process; rolls back if it dies or stays silent.
    pub fn on_handshake_timer(&mut self) {
        let Some((child, started, version)) = self.upd.child.as_mut() else {
            unsafe {
                let _ = KillTimer(self.hwnd, T_HANDSHAKE);
            }
            return;
        };
        let exited = matches!(child.try_wait(), Ok(Some(_)));
        if !exited && started.elapsed().as_secs() < 25 {
            return;
        }
        let version = version.clone();
        if !exited {
            let _ = child.kill();
        }
        self.upd.child = None;
        unsafe {
            let _ = KillTimer(self.hwnd, T_HANDSHAKE);
            let _ = ShowWindow(self.hwnd, SW_SHOW);
        }
        let back = update::roll_back();
        self.flash(&format!(
            "Slate {version} did not start correctly{}",
            if back { "; kept the current version" } else { "" }
        ));
    }

    /// The new process reported in: leave without touching the session.
    pub fn on_update_ok(&mut self) {
        unsafe {
            let _ = KillTimer(self.hwnd, T_HANDSHAKE);
        }
        self.upd.child = None;
        self.exiting_for_update = true;
        settings::save();
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    /// In the new process: tell the old window we are up, then tidy up.
    pub fn finish_handover(&mut self) {
        if let Some((old, from)) = self.upd.handover.take() {
            unsafe {
                let _ = PostMessageW(HWND(old as *mut _), WM_UPDATE_OK, WPARAM(0), LPARAM(0));
                SetTimer(self.hwnd, T_CLEANUP, 4000, None);
            }
            settings::get_mut().skip_version.clear();
            self.flash(&format!("Updated from {from} to {}", update::current_version()));
        }
    }

    pub fn on_cleanup_timer(&mut self) {
        unsafe {
            let _ = KillTimer(self.hwnd, T_CLEANUP);
        }
        update::cleanup();
    }
}

fn post(hwnd: isize, manual: bool, msg: Box<UpdateMsg>) {
    let p = Box::into_raw(msg);
    unsafe {
        if PostMessageW(HWND(hwnd as *mut _), WM_UPDATE, WPARAM(manual as usize), LPARAM(p as isize)).is_err() {
            drop(Box::from_raw(p));
        }
    }
}
