//! File bytes <-> internal UTF-8 with `\n` line endings.

use windows::Win32::Globalization::{MultiByteToWideChar, WideCharToMultiByte, CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Enc {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Ansi,
}

impl Enc {
    pub const ALL: [Enc; 5] = [Enc::Utf8, Enc::Utf8Bom, Enc::Utf16Le, Enc::Utf16Be, Enc::Ansi];
    pub fn name(self) -> &'static str {
        match self {
            Enc::Utf8 => "UTF-8",
            Enc::Utf8Bom => "UTF-8 BOM",
            Enc::Utf16Le => "UTF-16 LE",
            Enc::Utf16Be => "UTF-16 BE",
            Enc::Ansi => "ANSI",
        }
    }
    pub fn from_name(s: &str) -> Option<Enc> {
        Enc::ALL.into_iter().find(|e| e.name() == s)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Eol {
    Crlf,
    Lf,
    Cr,
}

impl Eol {
    pub const ALL: [Eol; 3] = [Eol::Crlf, Eol::Lf, Eol::Cr];
    pub fn name(self) -> &'static str {
        match self {
            Eol::Crlf => "CRLF",
            Eol::Lf => "LF",
            Eol::Cr => "CR",
        }
    }
    pub fn from_name(s: &str) -> Option<Eol> {
        Eol::ALL.into_iter().find(|e| e.name() == s)
    }
    fn bytes(self) -> &'static [u8] {
        match self {
            Eol::Crlf => b"\r\n",
            Eol::Lf => b"\n",
            Eol::Cr => b"\r",
        }
    }
}

pub struct Decoded {
    pub text: Vec<u8>,
    pub enc: Enc,
    pub eol: Eol,
    /// The file mixed line-ending styles; they were unified to `eol`.
    pub mixed_eol: bool,
}

pub fn detect(raw: &[u8]) -> Enc {
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Enc::Utf8Bom
    } else if raw.starts_with(&[0xFF, 0xFE]) {
        Enc::Utf16Le
    } else if raw.starts_with(&[0xFE, 0xFF]) {
        Enc::Utf16Be
    } else if std::str::from_utf8(raw).is_ok() {
        Enc::Utf8
    } else if looks_utf16le(raw) {
        Enc::Utf16Le
    } else {
        Enc::Ansi
    }
}

/// BOM-less UTF-16LE: mostly ASCII text leaves every odd byte zero.
fn looks_utf16le(raw: &[u8]) -> bool {
    let n = raw.len().min(4096) & !1;
    if n < 4 {
        return false;
    }
    let zeros = raw[..n].iter().skip(1).step_by(2).filter(|&&b| b == 0).count();
    zeros * 10 > (n / 2) * 9
}

/// Valid UTF-8 is taken over as is; only invalid input is copied.
fn utf8_owned(raw: Vec<u8>) -> Vec<u8> {
    match String::from_utf8(raw) {
        Ok(s) => s.into_bytes(),
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned().into_bytes(),
    }
}

/// Decodes file bytes, reusing the allocation for UTF-8 files so a large
/// file is never held twice.
pub fn decode(mut raw: Vec<u8>, force: Option<Enc>) -> Decoded {
    let enc = force.unwrap_or_else(|| detect(&raw));
    let text: Vec<u8> = match enc {
        Enc::Utf8 => utf8_owned(raw),
        Enc::Utf8Bom => {
            if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
                raw.drain(..3);
            }
            utf8_owned(raw)
        }
        Enc::Utf16Le | Enc::Utf16Be => {
            let mut body = &raw[..];
            if body.starts_with(&[0xFF, 0xFE]) || body.starts_with(&[0xFE, 0xFF]) {
                body = &body[2..];
            }
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|c| if enc == Enc::Utf16Le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
                .collect();
            String::from_utf16_lossy(&units).into_bytes()
        }
        Enc::Ansi => ansi_to_utf8(&raw),
    };
    let (text, eol, mixed_eol) = normalize_eol(text);
    Decoded { text, enc, eol, mixed_eol }
}

/// Converts CRLF and lone CR to LF, returning the dominant original style.
/// Text between carriage returns moves as whole runs, and an LF-only file
/// is left untouched.
pub fn normalize_eol(mut t: Vec<u8>) -> (Vec<u8>, Eol, bool) {
    use crate::doc::{each_byte, find_byte};
    let mut newlines = 0usize;
    each_byte(&t, b'\n', |_| newlines += 1);
    let Some(first) = find_byte(&t, b'\r') else {
        let eol = if newlines > 0 { Eol::Lf } else { Eol::Crlf };
        return (t, eol, false);
    };
    let (mut crlf, mut cr) = (0usize, 0usize);
    let n = t.len();
    let (mut w, mut r) = (first, first);
    while r < n {
        // t[r] is a carriage return.
        if r + 1 < n && t[r + 1] == b'\n' {
            crlf += 1;
            r += 2;
        } else {
            cr += 1;
            r += 1;
        }
        t[w] = b'\n';
        w += 1;
        let run = find_byte(&t[r..], b'\r').unwrap_or(n - r);
        t.copy_within(r..r + run, w);
        w += run;
        r += run;
    }
    t.truncate(w);
    let lf = newlines - crlf;
    let eol = if crlf == 0 && lf == 0 && cr == 0 {
        Eol::Crlf
    } else if crlf >= lf && crlf >= cr {
        Eol::Crlf
    } else if lf >= cr {
        Eol::Lf
    } else {
        Eol::Cr
    };
    let kinds = [crlf, lf, cr].iter().filter(|&&n| n > 0).count();
    (t, eol, kinds > 1)
}

pub fn encode(text: &[u8], enc: Enc, eol: Eol) -> Vec<u8> {
    let mut body = Vec::with_capacity(text.len() + text.len() / 32);
    if eol == Eol::Lf {
        body.extend_from_slice(text);
    } else {
        for &c in text {
            if c == b'\n' {
                body.extend_from_slice(eol.bytes());
            } else {
                body.push(c);
            }
        }
    }
    match enc {
        Enc::Utf8 => body,
        Enc::Utf8Bom => {
            let mut out = vec![0xEF, 0xBB, 0xBF];
            out.extend_from_slice(&body);
            out
        }
        Enc::Utf16Le | Enc::Utf16Be => {
            let s = String::from_utf8_lossy(&body);
            let mut out = if enc == Enc::Utf16Le { vec![0xFF, 0xFE] } else { vec![0xFE, 0xFF] };
            for u in s.encode_utf16() {
                out.extend_from_slice(&if enc == Enc::Utf16Le { u.to_le_bytes() } else { u.to_be_bytes() });
            }
            out
        }
        Enc::Ansi => utf8_to_ansi(&body),
    }
}

fn ansi_to_utf8(raw: &[u8]) -> Vec<u8> {
    if raw.is_empty() {
        return Vec::new();
    }
    unsafe {
        let n = MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), raw, None);
        let mut w = vec![0u16; n.max(0) as usize];
        MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), raw, Some(&mut w));
        String::from_utf16_lossy(&w).into_bytes()
    }
}

fn utf8_to_ansi(body: &[u8]) -> Vec<u8> {
    let w: Vec<u16> = String::from_utf8_lossy(body).encode_utf16().collect();
    if w.is_empty() {
        return Vec::new();
    }
    unsafe {
        let n = WideCharToMultiByte(CP_ACP, 0, &w, None, windows::core::PCSTR::null(), None);
        let mut out = vec![0u8; n.max(0) as usize];
        WideCharToMultiByte(CP_ACP, 0, &w, Some(&mut out), windows::core::PCSTR::null(), None);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eol_round_trip() {
        let d = decode(b"a\r\nb\r\nc".to_vec(), None);
        assert_eq!(d.text, b"a\nb\nc");
        assert_eq!(d.eol, Eol::Crlf);
        assert!(!d.mixed_eol);
        assert_eq!(encode(&d.text, d.enc, d.eol), b"a\r\nb\r\nc");
        let m = decode(b"a\nb\r\nc\rd".to_vec(), None);
        assert!(m.mixed_eol);
        assert_eq!(m.text, b"a\nb\nc\nd");
    }

    /// The run-based normaliser must match a plain byte-by-byte one.
    #[test]
    fn eol_normaliser_matches_reference() {
        fn reference(t: &[u8]) -> (Vec<u8>, usize, usize, usize) {
            let (mut out, mut crlf, mut lf, mut cr) = (Vec::new(), 0, 0, 0);
            let mut i = 0;
            while i < t.len() {
                match t[i] {
                    b'\r' if i + 1 < t.len() && t[i + 1] == b'\n' => {
                        crlf += 1;
                        out.push(b'\n');
                        i += 1;
                    }
                    b'\r' => {
                        cr += 1;
                        out.push(b'\n');
                    }
                    b'\n' => {
                        lf += 1;
                        out.push(b'\n');
                    }
                    c => out.push(c),
                }
                i += 1;
            }
            (out, crlf, lf, cr)
        }
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for len in [0usize, 1, 2, 7, 8, 9, 15, 16, 17, 63, 64, 65, 300, 4097] {
            for _ in 0..40 {
                let t: Vec<u8> = (0..len)
                    .map(|_| {
                        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                        [b'a', b'\r', b'\n', b'x', b'\r', 0xC3, 0xA9][(state >> 59) as usize % 7]
                    })
                    .collect();
                let (want, crlf, lf, cr) = reference(&t);
                let (got, eol, mixed) = normalize_eol(t.clone());
                assert_eq!(got, want, "len {len}");
                assert_eq!(mixed, [crlf, lf, cr].iter().filter(|&&n| n > 0).count() > 1);
                if crlf + lf + cr > 0 {
                    let best = *[crlf, lf, cr].iter().max().unwrap();
                    let n = match eol {
                        Eol::Crlf => crlf,
                        Eol::Lf => lf,
                        Eol::Cr => cr,
                    };
                    assert_eq!(n, best);
                }
                let mut naive = Vec::new();
                for (i, &b) in t.iter().enumerate() {
                    if b == b'\n' {
                        naive.push(i);
                    }
                }
                let mut fast = Vec::new();
                crate::doc::each_byte(&t, b'\n', |i| fast.push(i));
                assert_eq!(fast, naive);
                assert_eq!(crate::doc::find_byte(&t, b'\r'), t.iter().position(|&b| b == b'\r'));
            }
        }
    }

    #[test]
    fn utf16_round_trip() {
        let src = "hé€\n";
        let mut raw = vec![0xFF, 0xFE];
        for u in "hé€\r\n".encode_utf16() {
            raw.extend_from_slice(&u.to_le_bytes());
        }
        let d = decode(raw.clone(), None);
        assert_eq!(d.enc, Enc::Utf16Le);
        assert_eq!(d.text, src.as_bytes());
        assert_eq!(encode(&d.text, d.enc, d.eol), raw);
    }

    #[test]
    fn bom_and_ansi_detection() {
        assert_eq!(detect(&[0xEF, 0xBB, 0xBF, b'a']), Enc::Utf8Bom);
        assert_eq!(detect(b"plain"), Enc::Utf8);
        assert_eq!(detect(&[b'a', 0xE9, b'b']), Enc::Ansi);
    }
}
