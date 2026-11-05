//! Office-wide instant search: a trigram index of the project folders.
//!
//! One always-on PC runs `slate.exe --hub`, which walks the folders listed
//! in `<team folder>\index\roots.txt` and keeps `<team folder>\index\
//! search.idx` current, re-reading only files that changed. Every Slate
//! answers a search by intersecting the posting lists of the query's
//! trigrams and checking just those candidate files, instead of reading
//! the whole share.

use crate::search::Query;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAGIC: &[u8] = b"SLATEIDX1\n";
/// Larger files are listed by name but their text is not indexed.
const MAX_TEXT: u64 = 16 << 20;
/// Candidate files read to confirm a search; more means "refine it".
const MAX_VERIFY: usize = 3000;
const SKIP_DIRS: &[&str] = &[".git", ".svn", ".hg", "node_modules", "target", ".vs", "$RECYCLE.BIN", "System Volume Information"];

pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub mtime: u64,
    pub indexed: bool,
}

/// Sorted distinct trigrams of `text`, ASCII letters folded to lower case.
pub fn trigrams(text: &[u8], out: &mut Vec<u32>) {
    out.clear();
    if text.len() < 3 {
        return;
    }
    let low = |c: u8| c.to_ascii_lowercase() as u32;
    let mut t = (low(text[0]) << 8) | low(text[1]);
    for &c in &text[2..] {
        t = ((t << 8) | low(c)) & 0xFF_FFFF;
        out.push(t);
    }
    out.sort_unstable();
    out.dedup();
}

fn put_varint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn get_varints(b: &[u8], out: &mut Vec<u32>) {
    let (mut v, mut shift, mut last) = (0u32, 0, 0u32);
    for &c in b {
        v |= ((c & 0x7F) as u32) << shift;
        if c & 0x80 == 0 {
            last += v;
            out.push(last);
            v = 0;
            shift = 0;
        } else {
            shift += 7;
        }
    }
}

/// Writes an index of `files`; `sets[i]` holds file i's trigrams.
pub fn write_index(path: &Path, built: u64, files: &[FileEntry], sets: &[Vec<u32>]) -> std::io::Result<()> {
    let mut post: HashMap<u32, Vec<u32>> = HashMap::new();
    for (id, s) in sets.iter().enumerate() {
        for &t in s {
            post.entry(t).or_default().push(id as u32);
        }
    }
    let mut tris: Vec<u32> = post.keys().copied().collect();
    tris.sort_unstable();
    let mut head = Vec::new();
    head.extend_from_slice(MAGIC);
    head.extend_from_slice(&built.to_le_bytes());
    head.extend_from_slice(&(files.len() as u32).to_le_bytes());
    for f in files {
        head.extend_from_slice(&(f.path.len() as u32).to_le_bytes());
        head.extend_from_slice(f.path.as_bytes());
        head.extend_from_slice(&f.size.to_le_bytes());
        head.extend_from_slice(&f.mtime.to_le_bytes());
        head.push(f.indexed as u8);
    }
    head.extend_from_slice(&(tris.len() as u32).to_le_bytes());
    let mut body = Vec::new();
    for t in &tris {
        let ids = &post[t];
        let at = body.len() as u64;
        let mut prev = 0u32;
        for (k, &id) in ids.iter().enumerate() {
            put_varint(&mut body, if k == 0 { id } else { id - prev });
            prev = id;
        }
        head.extend_from_slice(&t.to_le_bytes());
        head.extend_from_slice(&at.to_le_bytes());
        head.extend_from_slice(&((body.len() as u64 - at) as u32).to_le_bytes());
    }
    let tmp = path.with_extension("idx.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&head)?;
        f.write_all(&body)?;
    }
    std::fs::rename(&tmp, path)
}

pub struct Index {
    file: std::fs::File,
    pub built: u64,
    pub files: Vec<FileEntry>,
    /// (trigram, offset into postings, byte length), sorted.
    dir: Vec<(u32, u64, u32)>,
    body_at: u64,
}

impl Index {
    pub fn open(path: &Path) -> std::io::Result<Index> {
        let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "not a Slate index");
        let mut file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        // Read the header in one go: names and the trigram directory.
        let mut r = std::io::BufReader::with_capacity(1 << 20, &mut file);
        let mut magic = [0u8; 10];
        r.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err(bad());
        }
        let mut b8 = [0u8; 8];
        let mut b4 = [0u8; 4];
        r.read_exact(&mut b8)?;
        let built = u64::from_le_bytes(b8);
        r.read_exact(&mut b4)?;
        let n = u32::from_le_bytes(b4) as usize;
        let mut files = Vec::with_capacity(n);
        let mut pos = 10 + 8 + 4u64;
        for _ in 0..n {
            r.read_exact(&mut b4)?;
            let l = u32::from_le_bytes(b4) as usize;
            if l > 32 * 1024 {
                return Err(bad());
            }
            let mut p = vec![0u8; l];
            r.read_exact(&mut p)?;
            r.read_exact(&mut b8)?;
            let size = u64::from_le_bytes(b8);
            r.read_exact(&mut b8)?;
            let mtime = u64::from_le_bytes(b8);
            let mut ix = [0u8; 1];
            r.read_exact(&mut ix)?;
            files.push(FileEntry { path: String::from_utf8(p).map_err(|_| bad())?, size, mtime, indexed: ix[0] != 0 });
            pos += 4 + l as u64 + 17;
        }
        r.read_exact(&mut b4)?;
        let nt = u32::from_le_bytes(b4) as usize;
        pos += 4;
        let mut dir = Vec::with_capacity(nt);
        let mut e = [0u8; 16];
        for _ in 0..nt {
            r.read_exact(&mut e)?;
            let t = u32::from_le_bytes(e[0..4].try_into().unwrap());
            let at = u64::from_le_bytes(e[4..12].try_into().unwrap());
            let bl = u32::from_le_bytes(e[12..16].try_into().unwrap());
            dir.push((t, at, bl));
        }
        pos += nt as u64 * 16;
        if pos > len {
            return Err(bad());
        }
        drop(r);
        Ok(Index { file, built, files, dir, body_at: pos })
    }

    pub fn postings(&mut self, t: u32) -> Vec<u32> {
        let Ok(k) = self.dir.binary_search_by_key(&t, |e| e.0) else { return Vec::new() };
        let (_, at, bl) = self.dir[k];
        let mut buf = vec![0u8; bl as usize];
        if self.file.seek(SeekFrom::Start(self.body_at + at)).is_err() || self.file.read_exact(&mut buf).is_err() {
            return Vec::new();
        }
        let mut out = Vec::new();
        get_varints(&buf, &mut out);
        out
    }

    /// Files that may contain `text` (case-insensitive): those holding all
    /// its trigrams. None when the text is too short to narrow anything.
    pub fn candidates(&mut self, text: &str) -> Option<Vec<u32>> {
        let mut tris = Vec::new();
        trigrams(text.as_bytes(), &mut tris);
        if tris.is_empty() {
            return None;
        }
        let mut lists: Vec<Vec<u32>> = tris.iter().map(|&t| self.postings(t)).collect();
        lists.sort_by_key(|l| l.len());
        let mut cur = lists[0].clone();
        for l in &lists[1..] {
            if cur.is_empty() {
                break;
            }
            let mut j = 0;
            cur.retain(|&id| {
                while j < l.len() && l[j] < id {
                    j += 1;
                }
                j < l.len() && l[j] == id
            });
        }
        Some(cur)
    }

    /// Every file's trigrams, rebuilt from the posting lists (the hub
    /// reuses them for files that did not change).
    pub fn file_sets(&mut self) -> Vec<Vec<u32>> {
        let mut sets = vec![Vec::new(); self.files.len()];
        let mut body = Vec::new();
        if self.file.seek(SeekFrom::Start(self.body_at)).is_err() || self.file.read_to_end(&mut body).is_err() {
            return sets;
        }
        let mut ids = Vec::new();
        for &(t, at, bl) in &self.dir {
            ids.clear();
            let s = at as usize;
            if s + bl as usize > body.len() {
                continue;
            }
            get_varints(&body[s..s + bl as usize], &mut ids);
            for &id in &ids {
                if let Some(v) = sets.get_mut(id as usize) {
                    v.push(t);
                }
            }
        }
        sets
    }
}

pub fn index_dir(team: &Path) -> PathBuf {
    team.join("index")
}

/// Folders to index, one per line in roots.txt (`#` starts a comment).
pub fn roots(team: &Path) -> Vec<PathBuf> {
    let t = std::fs::read_to_string(index_dir(team).join("roots.txt")).unwrap_or_default();
    // Notepad may save it with a byte-order mark.
    let t = t.trim_start_matches('\u{FEFF}');
    t.lines().map(|l| l.trim().trim_matches('"')).filter(|l| !l.is_empty() && !l.starts_with('#')).map(PathBuf::from).collect()
}

fn mtime_of(md: &std::fs::Metadata) -> u64 {
    md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

/// Text of a file for indexing; None for binary files.
fn text_of(p: &Path) -> Option<Vec<u8>> {
    let raw = std::fs::read(p).ok()?;
    let head = &raw[..raw.len().min(8000)];
    let enc = crate::enc::detect(&raw);
    let wide = matches!(enc, crate::enc::Enc::Utf16Le | crate::enc::Enc::Utf16Be);
    if head.contains(&0) && !wide {
        return None;
    }
    Some(if wide { crate::enc::decode(raw, None).text } else { raw })
}

/// One indexing pass; returns a status line.
pub fn build(team: &Path, log: &mut dyn FnMut(&str)) -> std::io::Result<String> {
    let dir = index_dir(team);
    std::fs::create_dir_all(&dir)?;
    let idx_path = dir.join("search.idx");
    let roots = roots(team);
    if roots.is_empty() {
        let note = dir.join("roots.txt");
        if !note.exists() {
            std::fs::write(&note, "# Folders to index for instant search, one per line, e.g.\n# \\\\server\\projects\\P12513\n")?;
        }
        return Ok(format!("No folders listed in {}", note.display()));
    }
    // What the last pass knew, to skip unchanged files.
    let mut old: HashMap<String, (u64, u64, bool, Vec<u32>)> = HashMap::new();
    if let Ok(mut ix) = Index::open(&idx_path) {
        let sets = ix.file_sets();
        for (f, s) in ix.files.into_iter().zip(sets) {
            old.insert(f.path, (f.size, f.mtime, f.indexed, s));
        }
    }
    let t0 = std::time::Instant::now();
    let mut files = Vec::new();
    let mut sets = Vec::new();
    let (mut read, mut reused) = (0usize, 0usize);
    let mut tris = Vec::new();
    for root in &roots {
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                let name = e.file_name().to_string_lossy().into_owned();
                if ft.is_dir() {
                    if !SKIP_DIRS.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
                        stack.push(e.path());
                    }
                    continue;
                }
                let Ok(md) = e.metadata() else { continue };
                let path = e.path().to_string_lossy().into_owned();
                let (size, mtime) = (md.len(), mtime_of(&md));
                let (indexed, set) = match old.remove(&path) {
                    Some((s, m, ix, set)) if s == size && m == mtime => {
                        reused += 1;
                        (ix, set)
                    }
                    _ if size <= MAX_TEXT => match text_of(&e.path()) {
                        Some(text) => {
                            read += 1;
                            trigrams(&text, &mut tris);
                            (true, tris.clone())
                        }
                        None => (false, Vec::new()),
                    },
                    _ => (false, Vec::new()),
                };
                files.push(FileEntry { path, size, mtime, indexed });
                sets.push(set);
                if files.len() % 5000 == 0 {
                    log(&format!("{} files so far ({read} read)", files.len()));
                }
            }
        }
    }
    write_index(&idx_path, crate::team::now(), &files, &sets)?;
    Ok(format!(
        "Indexed {} files ({} read, {} unchanged) in {:.1} s",
        files.len(),
        read,
        reused,
        t0.elapsed().as_secs_f32()
    ))
}

/// `slate.exe --hub [--once]`: keeps the index current, every 10 minutes.
pub fn hub_main(once: bool) -> i32 {
    let Some(team) = crate::team::team_dir() else { return 2 };
    let dir = index_dir(&team);
    let _ = std::fs::create_dir_all(&dir);
    // Only one hub per team folder: hold hub.lock open exclusively.
    use std::os::windows::fs::OpenOptionsExt;
    let Ok(_lock) = std::fs::OpenOptions::new().write(true).create(true).share_mode(0).open(dir.join("hub.lock")) else { return 3 };
    let status = dir.join("hub.txt");
    loop {
        let mut log = |s: &str| {
            let _ = std::fs::write(&status, format!("running: {s}\n"));
        };
        let line = match build(&team, &mut log) {
            Ok(s) => s,
            Err(e) => format!("error: {e}"),
        };
        let _ = std::fs::write(&status, format!("{line}\nat={}\n", crate::team::now()));
        if once {
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_secs(600));
    }
}

/// Searches through the index; results text in the find-in-files format.
pub fn search(team: &Path, q: &Query) -> String {
    let idx_path = index_dir(team).join("search.idx");
    let mut ix = match Index::open(&idx_path) {
        Ok(ix) => ix,
        Err(_) => {
            return format!(
                "Search \"{}\": no team index yet.\n  Run slate.exe --hub on an always-on PC, with the folders to index listed in {}\n",
                q.pat,
                index_dir(team).join("roots.txt").display()
            )
        }
    };
    let m = match q.compile() {
        Ok(m) => m,
        Err(e) => return format!("Search \"{}\": invalid pattern ({e})\n", q.pat),
    };
    let age = crate::team::now().saturating_sub(ix.built) / 60;
    let fresh = match age {
        0 => "index updated just now".to_string(),
        1 => "index updated 1 minute ago".to_string(),
        a if a < 120 => format!("index updated {a} minutes ago"),
        a => format!("index updated {} hours ago", a / 60),
    };
    // Literal text narrows through trigrams; a regex is checked everywhere.
    let lit = if q.regex { None } else { Some(q.pat.as_str()) };
    let cands: Vec<u32> = match lit.and_then(|t| ix.candidates(t)) {
        Some(c) => c,
        None => (0..ix.files.len() as u32).filter(|&i| ix.files[i as usize].indexed).collect(),
    };
    let needle = q.pat.to_lowercase();
    let mut body = String::new();
    // File names first.
    let mut names = 0;
    if !q.regex {
        for f in &ix.files {
            let name = Path::new(&f.path).file_name().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
            if name.contains(&needle) {
                if names == 0 {
                    body += "  File names:\n";
                }
                names += 1;
                if names <= 200 {
                    body += &format!("    {}\n", f.path);
                }
            }
        }
    }
    let (mut hits, mut files_hit, mut checked) = (0usize, 0usize, 0usize);
    let capped = cands.len() > MAX_VERIFY;
    for &id in cands.iter().take(MAX_VERIFY) {
        let f = &ix.files[id as usize];
        checked += 1;
        if let Some((n, lines)) = crate::search::search_path(Path::new(&f.path), &m) {
            files_hit += 1;
            hits += n;
            body += &format!("  {} ({n} hit{})\n", f.path, if n == 1 { "" } else { "s" });
            body += &lines;
        }
    }
    let more = if capped { format!("; {} more candidate files not checked \u{2013} search for something more specific", cands.len() - MAX_VERIFY) } else { String::new() };
    format!(
        "Search \"{}\" ({hits} hit{} in {files_hit} file{}, {checked} checked of {} indexed{more}; {fresh})\n{body}",
        q.pat,
        if hits == 1 { "" } else { "s" },
        if files_hit == 1 { "" } else { "s" },
        ix.files.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigram_index_round_trip() {
        let dir = std::env::temp_dir().join(format!("slate-idx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let texts = ["PIPE ELBOW 90", "flange FW1A06", "elbow and Pipe support"];
        let mut files = Vec::new();
        let mut sets = Vec::new();
        for (k, t) in texts.iter().enumerate() {
            let mut s = Vec::new();
            trigrams(t.as_bytes(), &mut s);
            files.push(FileEntry { path: format!("f{k}.txt"), size: t.len() as u64, mtime: k as u64, indexed: true });
            sets.push(s);
        }
        let p = dir.join("search.idx");
        write_index(&p, 77, &files, &sets).unwrap();
        let mut ix = Index::open(&p).unwrap();
        assert_eq!(ix.built, 77);
        assert_eq!(ix.files.len(), 3);
        assert_eq!(ix.candidates("elbow"), Some(vec![0, 2]));
        assert_eq!(ix.candidates("fw1a"), Some(vec![1]));
        assert_eq!(ix.candidates("zzz"), Some(vec![]));
        assert_eq!(ix.candidates("el"), None);
        let back = ix.file_sets();
        assert_eq!(back, sets);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn varints() {
        let mut b = Vec::new();
        for v in [5u32, 1, 300, 70000] {
            put_varint(&mut b, v);
        }
        let mut out = Vec::new();
        get_varints(&b, &mut out);
        assert_eq!(out, vec![5, 6, 306, 70306]);
    }
}
