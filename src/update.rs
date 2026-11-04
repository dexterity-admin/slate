//! Self-update from GitHub Releases.
//!
//! Every release publishes `update.json` (version, exe size and SHA-256) and
//! `update.json.sig` (ECDSA P-256 over the manifest bytes, raw r||s). Slate
//! downloads both from the `releases/latest/download/` redirect, which is a
//! CDN download rather than the rate-limited REST API, verifies the
//! signature with the public key compiled in, then downloads the exe and
//! checks its size and hash before touching anything on disk.
//!
//! Installing renames the running exe to `<exe>.old`, moves the new one into
//! place and starts it with `--after-update`. The old process stays hidden
//! until the new one reports in; if it does not, the files are swapped back.

use std::path::{Path, PathBuf};
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptDestroyKey, BCryptHash, BCryptImportKeyPair, BCryptOpenAlgorithmProvider,
    BCryptVerifySignature, BCRYPT_ALG_HANDLE, BCRYPT_ECCPUBLIC_BLOB, BCRYPT_ECDSA_P256_ALGORITHM, BCRYPT_FLAGS,
    BCRYPT_KEY_HANDLE, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALG_HANDLE,
};

pub const REPO: &str = "dexterity-admin/slate";
const PUBLIC_KEY: [u8; 64] = include!("update_key.in");
const MAX_MANIFEST: usize = 64 * 1024;
const MAX_EXE: usize = 64 << 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub version: String,
    pub size: usize,
    pub sha256: String,
    pub notes: String,
}

#[derive(Debug)]
pub enum UpdateMsg {
    /// A verified newer release exists.
    Available(Manifest),
    /// Checked; nothing newer.
    UpToDate,
    /// The new exe is downloaded and verified at this path.
    Ready(Manifest, PathBuf),
    Failed(String),
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// "1.10.2" > "1.9.9"; suffixes after '-' are ignored.
pub fn newer(candidate: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<(u32, u32, u32)> {
        let core = v.trim().trim_start_matches('v').split('-').next()?;
        let mut it = core.split('.').map(|p| p.parse::<u32>());
        Some((it.next()?.ok()?, it.next().unwrap_or(Ok(0)).ok()?, it.next().unwrap_or(Ok(0)).ok()?))
    }
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Minimal JSON field reader for the flat manifest Slate itself publishes.
fn json_field(s: &str, key: &str) -> Option<String> {
    let k = format!("\"{key}\"");
    let at = s.find(&k)? + k.len();
    let rest = s[at..].trim_start().strip_prefix(':')?.trim_start();
    if let Some(r) = rest.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = r.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    c => out.push(c),
                },
                c => out.push(c),
            }
        }
        None
    } else {
        Some(rest.chars().take_while(|c| c.is_ascii_digit()).collect())
    }
}

pub fn parse_manifest(text: &str) -> Option<Manifest> {
    let m = Manifest {
        version: json_field(text, "version")?,
        size: json_field(text, "size")?.parse().ok()?,
        sha256: json_field(text, "sha256")?.to_ascii_lowercase(),
        notes: json_field(text, "notes").unwrap_or_default(),
    };
    (m.sha256.len() == 64 && m.size > 0 && m.size <= MAX_EXE).then_some(m)
}

// ---- crypto (Windows CNG) -------------------------------------------------------

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    unsafe {
        let _ = BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out);
    }
    out
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Verifies a raw r||s ECDSA P-256 signature over SHA-256(`data`).
pub fn verify(data: &[u8], sig: &[u8], key: &[u8; 64]) -> bool {
    if sig.len() != 64 {
        return false;
    }
    let mut blob = Vec::with_capacity(8 + 64);
    blob.extend_from_slice(&0x3153_4345u32.to_le_bytes()); // BCRYPT_ECDSA_PUBLIC_P256_MAGIC
    blob.extend_from_slice(&32u32.to_le_bytes());
    blob.extend_from_slice(key);
    let digest = sha256(data);
    unsafe {
        let mut alg = BCRYPT_ALG_HANDLE::default();
        if BCryptOpenAlgorithmProvider(&mut alg, BCRYPT_ECDSA_P256_ALGORITHM, None, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0)).is_err() {
            return false;
        }
        let mut k = BCRYPT_KEY_HANDLE::default();
        let ok = BCryptImportKeyPair(alg, None, BCRYPT_ECCPUBLIC_BLOB, &mut k, &blob, 0).is_ok()
            && BCryptVerifySignature(k, None, &digest, sig, BCRYPT_FLAGS(0)).is_ok();
        let _ = BCryptDestroyKey(k);
        let _ = BCryptCloseAlgorithmProvider(alg, 0);
        ok
    }
}

fn b64decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let clean: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = Vec::new();
    for chunk in clean.chunks(4) {
        let mut acc = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            acc |= val(c)? << (18 - 6 * i);
        }
        let n = chunk.len() * 6 / 8;
        out.extend_from_slice(&acc.to_be_bytes()[1..1 + n]);
    }
    Some(out)
}

// ---- HTTP (WinHTTP, system proxy aware) ----------------------------------------

struct H(*mut std::ffi::c_void);
impl Drop for H {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// GET over HTTPS following redirects; refuses bodies over `limit`.
pub fn http_get(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let rest = url.strip_prefix("https://").ok_or("only https is allowed")?;
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
    let wh = crate::sys::wide(host);
    let wp = crate::sys::wide(&path);
    let agent = crate::sys::wide(&format!("Slate/{}", current_version()));
    unsafe {
        let s = H(WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if s.0.is_null() {
            return Err("network unavailable".into());
        }
        let _ = WinHttpSetTimeouts(s.0, 10_000, 10_000, 15_000, 30_000);
        let c = H(WinHttpConnect(s.0, PCWSTR(wh.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT as u16, 0));
        if c.0.is_null() {
            return Err("cannot reach the update server".into());
        }
        let r = H(WinHttpOpenRequest(c.0, w!("GET"), PCWSTR(wp.as_ptr()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE));
        if r.0.is_null() {
            return Err("request failed".into());
        }
        WinHttpSendRequest(r.0, None, None, 0, 0, 0).map_err(|e| format!("cannot reach the update server ({})", e.code().0 & 0xFFFF))?;
        WinHttpReceiveResponse(r.0, std::ptr::null_mut()).map_err(|_| "no response from the update server".to_string())?;
        let mut status = 0u32;
        let mut len = 4u32;
        let _ = WinHttpQueryHeaders(
            r.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            std::ptr::null_mut(),
        );
        if status != 200 {
            return Err(if status == 404 { "no release published yet".into() } else { format!("server answered {status}") });
        }
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut got = 0u32;
            WinHttpReadData(r.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut got).map_err(|_| "download interrupted".to_string())?;
            if got == 0 {
                break;
            }
            out.extend_from_slice(&buf[..got as usize]);
            if out.len() > limit {
                return Err("download larger than expected".into());
            }
        }
        Ok(out)
    }
}

fn asset_url(name: &str) -> String {
    format!("https://github.com/{REPO}/releases/latest/download/{name}")
}

/// Fetches and verifies the latest manifest.
pub fn check() -> UpdateMsg {
    let manifest = match http_get(&asset_url("update.json"), MAX_MANIFEST) {
        Ok(m) => m,
        Err(e) => return UpdateMsg::Failed(e),
    };
    let sig = match http_get(&asset_url("update.json.sig"), 1024) {
        Ok(s) => s,
        Err(e) => return UpdateMsg::Failed(e),
    };
    let sig = b64decode(&String::from_utf8_lossy(&sig)).unwrap_or_default();
    if !verify(&manifest, &sig, &PUBLIC_KEY) {
        return UpdateMsg::Failed("the update manifest failed its signature check".into());
    }
    let Some(m) = parse_manifest(&String::from_utf8_lossy(&manifest)) else {
        return UpdateMsg::Failed("the update manifest is malformed".into());
    };
    if newer(&m.version, current_version()) {
        UpdateMsg::Available(m)
    } else {
        UpdateMsg::UpToDate
    }
}

pub fn staging_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Slate").join("update")
}

/// Downloads the exe for `m`, verifies it and leaves it in the staging folder.
pub fn download(m: &Manifest) -> UpdateMsg {
    let dir = staging_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return UpdateMsg::Failed(format!("cannot create {}: {e}", dir.display()));
    }
    let url = format!("https://github.com/{REPO}/releases/download/v{}/slate.exe", m.version);
    let data = match http_get(&url, m.size + 1) {
        Ok(d) => d,
        Err(e) => return UpdateMsg::Failed(e),
    };
    if data.len() != m.size || hex(&sha256(&data)) != m.sha256 {
        return UpdateMsg::Failed("the downloaded file does not match the signed manifest".into());
    }
    let part = dir.join(format!("slate-{}.exe.partial", m.version));
    let done = dir.join(format!("slate-{}.exe", m.version));
    if let Err(e) = std::fs::write(&part, &data).and_then(|_| std::fs::rename(&part, &done)) {
        let _ = std::fs::remove_file(&part);
        return UpdateMsg::Failed(format!("cannot save the update: {e}"));
    }
    UpdateMsg::Ready(m.clone(), done)
}

pub fn old_path(exe: &Path) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(".old");
    PathBuf::from(s)
}

/// Swaps `new_exe` into the place of the running exe. On error nothing changes.
pub fn swap_in(new_exe: &Path) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = old_path(&exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| format!("Slate's folder is not writable ({e}). Download the new version from the releases page instead."))?;
    if let Err(e) = std::fs::copy(new_exe, &exe) {
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("could not place the new version: {e}"));
    }
    Ok(exe)
}

/// Puts the previous exe back after a failed start.
pub fn roll_back() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    // current_exe() still names the original path for the renamed image.
    let target = if exe.extension().map(|e| e == "old").unwrap_or(false) { exe.with_extension("") } else { exe.clone() };
    let old = old_path(&target);
    if !old.exists() {
        return false;
    }
    let _ = std::fs::remove_file(&target);
    std::fs::rename(&old, &target).is_ok()
}

/// Removes leftovers from a finished update (best effort; retried each start).
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
    if let Ok(rd) = std::fs::read_dir(staging_dir()) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("1.1.0", "1.0.9"));
        assert!(newer("v1.10.0", "1.9.9"));
        assert!(!newer("1.0.0", "1.0.0"));
        assert!(!newer("0.9", "1.0.0"));
        assert!(!newer("garbage", "1.0.0"));
    }

    #[test]
    fn manifest_and_base64() {
        let m = parse_manifest(r#"{"version": "1.2.0", "size": 602112, "sha256": "AB12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12", "notes": "Fixes \"x\"\nmore"}"#).unwrap();
        assert_eq!(m.version, "1.2.0");
        assert_eq!(m.size, 602112);
        assert!(m.sha256.starts_with("ab12"));
        assert_eq!(m.notes, "Fixes \"x\"\nmore");
        assert!(parse_manifest(r#"{"version":"1","size":0,"sha256":"x"}"#).is_none());
        assert_eq!(b64decode("aGVsbG8=").unwrap(), b"hello");
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    /// Cross-check with tools/make_manifest.py: set SLATE_SIGNED_DIR to a folder
    /// holding update.json and update.json.sig made with the real key.
    #[test]
    fn accepts_manifest_signed_by_release_script() {
        let Some(dir) = std::env::var_os("SLATE_SIGNED_DIR") else { return };
        let dir = PathBuf::from(dir);
        let manifest = std::fs::read(dir.join("update.json")).unwrap();
        let sig = b64decode(&std::fs::read_to_string(dir.join("update.json.sig")).unwrap()).unwrap();
        assert!(verify(&manifest, &sig, &PUBLIC_KEY));
        let mut tampered = manifest.clone();
        tampered[10] ^= 1;
        assert!(!verify(&tampered, &sig, &PUBLIC_KEY));
        assert!(parse_manifest(&String::from_utf8_lossy(&manifest)).is_some());
    }

    #[test]
    fn rejects_bad_signature() {
        assert!(!verify(b"data", &[0u8; 64], &PUBLIC_KEY));
        assert!(!verify(b"data", &[0u8; 10], &PUBLIC_KEY));
    }
}
