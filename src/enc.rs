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

pub fn decode(raw: &[u8], force: Option<Enc>) -> Decoded {
    let enc = force.unwrap_or_else(|| detect(raw));
    let text: Vec<u8> = match enc {
        Enc::Utf8 => String::from_utf8_lossy(raw).into_owned().into_bytes(),
        Enc::Utf8Bom => {
            let body = raw.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(raw);
            String::from_utf8_lossy(body).into_owned().into_bytes()
        }
        Enc::Utf16Le | Enc::Utf16Be => {
            let mut body = raw;
            if body.starts_with(&[0xFF, 0xFE]) || body.starts_with(&[0xFE, 0xFF]) {
                body = &body[2..];
            }
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|c| if enc == Enc::Utf16Le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
                .collect();
            String::from_utf16_lossy(&units).into_bytes()
        }
        Enc::Ansi => ansi_to_utf8(raw),
    };
    let (text, eol, mixed_eol) = normalize_eol(text);
    Decoded { text, enc, eol, mixed_eol }
}

/// Converts CRLF and lone CR to LF, returning the dominant original style.
pub fn normalize_eol(mut t: Vec<u8>) -> (Vec<u8>, Eol, bool) {
    let (mut crlf, mut lf, mut cr) = (0usize, 0usize, 0usize);
    let mut w = 0;
    let mut r = 0;
    while r < t.len() {
        let c = t[r];
        if c == b'\r' {
            if r + 1 < t.len() && t[r + 1] == b'\n' {
                crlf += 1;
                r += 1;
            } else {
                cr += 1;
            }
            t[w] = b'\n';
        } else {
            if c == b'\n' {
                lf += 1;
            }
            t[w] = c;
        }
        w += 1;
        r += 1;
    }
    t.truncate(w);
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
        let d = decode(b"a\r\nb\r\nc", None);
        assert_eq!(d.text, b"a\nb\nc");
        assert_eq!(d.eol, Eol::Crlf);
        assert!(!d.mixed_eol);
        assert_eq!(encode(&d.text, d.enc, d.eol), b"a\r\nb\r\nc");
        let m = decode(b"a\nb\r\nc\rd", None);
        assert!(m.mixed_eol);
        assert_eq!(m.text, b"a\nb\nc\nd");
    }

    #[test]
    fn utf16_round_trip() {
        let src = "hé€\n";
        let mut raw = vec![0xFF, 0xFE];
        for u in "hé€\r\n".encode_utf16() {
            raw.extend_from_slice(&u.to_le_bytes());
        }
        let d = decode(&raw, None);
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
