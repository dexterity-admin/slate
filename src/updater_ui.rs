//! Updater glue: background checks, the status-bar notice, install and the
//! hand-over to the new process (with roll-back if it never reports in).

use crate::app::App;
use crate::settings;
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
                }
            }
            UpdateMsg::Failed(e) => {
                // Background checks fail quietly (offline, no release yet).
                if manual {
                    self.flash(&format!("Update check failed: {e}"));
                }
            }
            UpdateMsg::Ready(m, path) => self.apply_update(m, path),
        }
        self.redraw();
    }

    /// Asks, then downloads and installs the available update.
    pub fn install_update(&mut self) {
        let Some(m) = self.upd.available.clone() else {
            self.start_check(true);
            return;
        };
        let notes = if m.notes.is_empty() { String::new() } else { format!("\n\nWhat's new:\n{}", m.notes) };
        let text = format!(
            "Install Slate {} now? (you have {})\n\nSlate restarts and reopens all your tabs, including unsaved text.{notes}\n\nChoose No to be reminded later, or Cancel to skip this version.",
            m.version,
            update::current_version()
        );
        match sys::ask_yes_no_cancel(self.hwnd, &text) {
            sys::Answer::Yes => {}
            sys::Answer::No => return,
            sys::Answer::Cancel => {
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
