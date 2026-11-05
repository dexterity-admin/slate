//! Settings and session, stored as plain `key=value` text under %APPDATA%\Slate.

use std::path::PathBuf;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub struct Settings {
    pub dark: bool,
    pub font: String,
    pub font_size: f32,
    pub zoom: i32,
    pub wrap: bool,
    pub tab_size: usize,
    pub use_tabs: bool,
    pub show_ws: bool,
    pub restore_session: bool,
    pub auto_update: bool,
    /// Index into gfx::ACCENTS.
    pub accent: usize,
    pub animate: bool,
    /// Open .csv / .tsv files in the grid.
    pub csv_grid: bool,
    /// Compare ignores whitespace / letter case.
    pub cmp_ws: bool,
    pub cmp_case: bool,
    /// Release the user chose to skip.
    pub skip_version: String,
    pub recent: Vec<String>,
    pub win: Option<(i32, i32, i32, i32, bool)>,
}

static S: RwLock<Settings> = RwLock::new(Settings {
    dark: true,
    font: String::new(),
    font_size: 14.0,
    zoom: 0,
    wrap: false,
    tab_size: 4,
    use_tabs: false,
    show_ws: false,
    restore_session: true,
    auto_update: true,
    accent: 0,
    animate: true,
    csv_grid: true,
    cmp_ws: false,
    cmp_case: false,
    skip_version: String::new(),
    recent: Vec::new(),
    win: None,
});

pub fn get() -> RwLockReadGuard<'static, Settings> {
    S.read().unwrap_or_else(|e| e.into_inner())
}

pub fn get_mut() -> RwLockWriteGuard<'static, Settings> {
    S.write().unwrap_or_else(|e| e.into_inner())
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Slate")
}

pub fn backup_dir() -> PathBuf {
    dir().join("backup")
}

pub fn parse(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|l| {
        let l = l.trim_end_matches('\r');
        if l.starts_with('#') {
            return None;
        }
        l.split_once('=').map(|(k, v)| (k.trim(), v))
    })
}

pub fn load() {
    let text = std::fs::read_to_string(dir().join("settings.ini")).unwrap_or_default();
    let mut s = get_mut();
    s.font = "Cascadia Mono".into();
    s.dark = system_dark();
    for (k, v) in parse(&text) {
        match k {
            "theme" => s.dark = v != "light",
            "font" if !v.is_empty() => s.font = v.into(),
            "font_size" => s.font_size = v.parse().unwrap_or(14.0f32).clamp(6.0, 48.0),
            "zoom" => s.zoom = v.parse().unwrap_or(0i32).clamp(-8, 30),
            "wrap" => s.wrap = v == "1",
            "tab_size" => s.tab_size = v.parse().unwrap_or(4usize).clamp(1, 16),
            "use_tabs" => s.use_tabs = v == "1",
            "show_ws" => s.show_ws = v == "1",
            "restore_session" => s.restore_session = v != "0",
            "auto_update" => s.auto_update = v != "0",
            "accent" => s.accent = v.parse().unwrap_or(0usize).min(crate::gfx::ACCENTS.len() - 1),
            "animate" => s.animate = v != "0",
            "csv_grid" => s.csv_grid = v != "0",
            "cmp_ws" => s.cmp_ws = v == "1",
            "cmp_case" => s.cmp_case = v == "1",
            "skip_version" => s.skip_version = v.into(),
            "recent" if !v.is_empty() => s.recent.push(v.into()),
            "window" => {
                let p: Vec<i32> = v.split(',').filter_map(|x| x.parse().ok()).collect();
                if p.len() == 5 {
                    s.win = Some((p[0], p[1], p[2], p[3], p[4] != 0));
                }
            }
            _ => {}
        }
    }
}

pub fn save() {
    let s = get();
    let mut out = String::from("# Slate settings\n");
    let b = |v: bool| if v { "1" } else { "0" };
    out += &format!("theme={}\n", if s.dark { "dark" } else { "light" });
    out += &format!("font={}\nfont_size={}\nzoom={}\n", s.font, s.font_size, s.zoom);
    out += &format!("wrap={}\ntab_size={}\nuse_tabs={}\n", b(s.wrap), s.tab_size, b(s.use_tabs));
    out += &format!("show_ws={}\nrestore_session={}\n", b(s.show_ws), b(s.restore_session));
    out += &format!("auto_update={}\nskip_version={}\n", b(s.auto_update), s.skip_version);
    out += &format!("accent={}\nanimate={}\ncsv_grid={}\n", s.accent, b(s.animate), b(s.csv_grid));
    out += &format!("cmp_ws={}\ncmp_case={}\n", b(s.cmp_ws), b(s.cmp_case));
    if let Some((x, y, w, h, m)) = s.win {
        out += &format!("window={x},{y},{w},{h},{}\n", m as i32);
    }
    for r in &s.recent {
        out += &format!("recent={r}\n");
    }
    let _ = std::fs::create_dir_all(dir());
    let _ = std::fs::write(dir().join("settings.ini"), out);
}

pub fn push_recent(path: &str) {
    let mut s = get_mut();
    s.recent.retain(|r| !r.eq_ignore_ascii_case(path));
    s.recent.insert(0, path.to_string());
    s.recent.truncate(15);
}

fn system_dark() -> bool {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut v: u32 = 1;
    let mut n = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut n),
        )
    };
    // Default to dark when the value is absent.
    r.is_err() || v == 0
}
