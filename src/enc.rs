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

#[cfg(test)]
pub fn encode(text: &[u8], enc: Enc, eol: Eol) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + text.len() / 32 + 4);
    let _ = write_encoded(&mut out, [text, &[]], enc, eol, &mut |_| {});
    out
}

pub fn write_encoded(w: &mut impl std::io::Write, parts: [&[u8]; 2], enc: Enc, eol: Eol, done: &mut dyn FnMut(usize)) -> std::io::Result<()> {
    use crate::doc::find_byte;
    const BLOCK: usize = 4 << 20;
    match enc {
        Enc::Utf8Bom => w.write_all(&[0xEF, 0xBB, 0xBF])?,
        Enc::Utf16Le => w.write_all(&[0xFF, 0xFE])?,
        Enc::Utf16Be => w.write_all(&[0xFE, 0xFF])?,
        _ => {}
    }
    let mut lines = Vec::with_capacity(BLOCK + BLOCK / 16);
    let mut wide = Vec::new();
    let mut total = 0;
    for part in parts {
        let mut i = 0;
        while i < part.len() {
            // Blocks end on a character start, so no character is split.
            let mut e = (i + BLOCK).min(part.len());
            while e < part.len() && part[e] & 0xC0 == 0x80 {
                e += 1;
            }
            let block = &part[i..e];
            let text: &[u8] = if eol == Eol::Lf {
                block
            } else {
                lines.clear();
                let mut k = 0;
                while k < block.len() {
                    let run = find_byte(&block[k..], b'\n').unwrap_or(block.len() - k);
                    lines.extend_from_slice(&block[k..k + run]);
                    k += run;
                    if k < block.len() {
                        lines.extend_from_slice(eol.bytes());
                        k += 1;
                    }
                }
                &lines
            };
            match enc {
                Enc::Utf8 | Enc::Utf8Bom => w.write_all(text)?,
                Enc::Utf16Le | Enc::Utf16Be => {
                    wide.clear();
                    for u in String::from_utf8_lossy(text).encode_utf16() {
                        wide.extend_from_slice(&if enc == Enc::Utf16Le { u.to_le_bytes() } else { u.to_be_bytes() });
                    }
                    w.write_all(&wide)?;
                }
                Enc::Ansi => w.write_all(&utf8_to_ansi(text))?,
            }
            total += e - i;
            done(total);
            i = e;
        }
    }
    w.flush()
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

/// Decodes a file that arrives in pieces (a big file streaming in): the
/// same result as `decode` on the whole file, piece by piece. Characters
/// and CR LF pairs split across pieces are carried over.
pub struct StreamDecoder {
    pub enc: Option<Enc>,
    carry: Vec<u8>,
    pending_cr: bool,
    started: bool,
    crlf: usize,
    lf: usize,
    cr: usize,
}

/// The guessed UTF-8 turned out wrong further in: decode again as ANSI.
#[derive(Debug, PartialEq)]
pub struct NotUtf8;

impl StreamDecoder {
    pub fn new(force: Option<Enc>) -> StreamDecoder {
        StreamDecoder { enc: force, carry: Vec::new(), pending_cr: false, started: false, crlf: 0, lf: 0, cr: 0 }
    }

    /// Carries on decoding a file already read up to some point, in its
    /// known encoding. `ends_cr` says the bytes so far ended with a CR (its
    /// LF may come next and must not make a second line break).
    pub fn resume(enc: Enc, ends_cr: bool) -> StreamDecoder {
        StreamDecoder { enc: Some(enc), carry: Vec::new(), pending_cr: ends_cr, started: true, crlf: 0, lf: 0, cr: 0 }
    }

    /// The next piece, decoded to UTF-8 with line endings made `\n`.
    /// `last` marks the end of the file.
    pub fn push(&mut self, raw: &[u8], last: bool) -> Result<Vec<u8>, NotUtf8> {
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(raw);
        if !self.started {
            // Decide the encoding from the first piece (and strip a BOM).
            if data.len() < 4 && !last {
                self.carry = data;
                return Ok(Vec::new());
            }
            self.started = true;
            let enc = match self.enc {
                Some(e) => e,
                None => {
                    if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
                        Enc::Utf8Bom
                    } else if data.starts_with(&[0xFF, 0xFE]) {
                        Enc::Utf16Le
                    } else if data.starts_with(&[0xFE, 0xFF]) {
                        Enc::Utf16Be
                    } else if utf8_prefix_ok(&data, last) {
                        Enc::Utf8
                    } else if looks_utf16le(&data) {
                        Enc::Utf16Le
                    } else {
                        Enc::Ansi
                    }
                }
            };
            self.enc = Some(enc);
            let bom = match enc {
                Enc::Utf8Bom if data.starts_with(&[0xEF, 0xBB, 0xBF]) => 3,
                Enc::Utf16Le if data.starts_with(&[0xFF, 0xFE]) => 2,
                Enc::Utf16Be if data.starts_with(&[0xFE, 0xFF]) => 2,
                _ => 0,
            };
            data.drain(..bom);
        }
        let text = match self.enc.unwrap() {
            Enc::Utf8 | Enc::Utf8Bom => match std::str::from_utf8(&data) {
                Ok(_) => data,
                // Cut inside a character at the end of the piece: carry it.
                Err(e) if e.error_len().is_none() && !last => {
                    let ok = e.valid_up_to();
                    self.carry = data[ok..].to_vec();
                    data.truncate(ok);
                    data
                }
                Err(_) => {
                    if self.enc == Some(Enc::Utf8Bom) {
                        String::from_utf8_lossy(&data).into_owned().into_bytes()
                    } else {
                        return Err(NotUtf8);
                    }
                }
            },
            e @ (Enc::Utf16Le | Enc::Utf16Be) => {
                let even = data.len() & !1;
                let mut units: Vec<u16> = data[..even]
                    .chunks_exact(2)
                    .map(|c| if e == Enc::Utf16Le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
                    .collect();
                let mut keep = data[even..].to_vec();
                // A high surrogate at the end waits for its partner.
                if !last && units.last().map_or(false, |&u| (0xD800..0xDC00).contains(&u)) {
                    let u = units.pop().unwrap();
                    let b = if e == Enc::Utf16Le { u.to_le_bytes() } else { u.to_be_bytes() };
                    let mut k = b.to_vec();
                    k.extend_from_slice(&keep);
                    keep = k;
                }
                if !last {
                    self.carry = keep;
                }
                String::from_utf16_lossy(&units).into_bytes()
            }
            Enc::Ansi => {
                // A double-byte lead at the end waits for its second byte.
                if !last && data.last().map_or(false, |&b| unsafe { windows::Win32::Globalization::IsDBCSLeadByte(b).is_ok() }) {
                    self.carry = vec![data.pop().unwrap()];
                }
                ansi_to_utf8(&data)
            }
        };
        Ok(self.lines(text, last))
    }

    /// Makes every line ending `\n`, counting each kind.
    fn lines(&mut self, t: Vec<u8>, last: bool) -> Vec<u8> {
        use crate::doc::{each_byte, find_byte};
        let mut out = Vec::with_capacity(t.len());
        let mut i = 0;
        // Nothing new yet (the piece was carried): a pending CR still waits.
        if t.is_empty() && !last {
            return out;
        }
        if self.pending_cr {
            self.pending_cr = false;
            if t.first() == Some(&b'\n') {
                self.crlf += 1;
                i = 1;
            } else {
                self.cr += 1;
            }
        }
        while i < t.len() {
            let run = find_byte(&t[i..], b'\r').unwrap_or(t.len() - i);
            let seg = &t[i..i + run];
            each_byte(seg, b'\n', |_| self.lf += 1);
            out.extend_from_slice(seg);
            i += run;
            if i >= t.len() {
                break;
            }
            // t[i] is a carriage return.
            out.push(b'\n');
            if i + 1 < t.len() {
                if t[i + 1] == b'\n' {
                    self.crlf += 1;
                    i += 2;
                } else {
                    self.cr += 1;
                    i += 1;
                }
            } else if last {
                self.cr += 1;
                i += 1;
            } else {
                // Is it CR LF? The next piece says.
                self.pending_cr = true;
                i += 1;
            }
        }
        out
    }

    /// Encoding, dominant line ending and whether endings were mixed.
    pub fn finish(&self) -> (Enc, Eol, bool) {
        let (crlf, lf, cr) = (self.crlf, self.lf, self.cr + self.pending_cr as usize);
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
        (self.enc.unwrap_or(Enc::Utf8), eol, kinds > 1)
    }
}

/// Whether `data` is UTF-8, allowing a character cut off at the end.
fn utf8_prefix_ok(data: &[u8], last: bool) -> bool {
    match std::str::from_utf8(data) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && !last,
    }
}

#[cfg(test)]
mod tests {

    /// Streaming in random pieces gives what decoding the whole file gives.
    #[test]
    fn stream_decoder_matches_whole_file_decode() {
        let samples: Vec<Vec<u8>> = vec![
            "PIPE\r\nELBOW 90\r\n6\" FLANGE\r\nend".as_bytes().to_vec(),
            "unix\nlines\nonly\n".as_bytes().to_vec(),
            "old\rmac\rfile".as_bytes().to_vec(),
            "mixed\r\nends\nhere\rand\r\n".as_bytes().to_vec(),
            "caf\u{e9} \u{2192} \u{1F600} done\r\n".as_bytes().to_vec(),
            [&[0xEF, 0xBB, 0xBF][..], "bom \u{e9}\r\nx".as_bytes()].concat(),
            {
                let mut v = vec![0xFF, 0xFE];
                for u in "wide \u{1F600} text\r\nline 2".encode_utf16() {
                    v.extend_from_slice(&u.to_le_bytes());
                }
                v
            },
            b"plain ascii\r\n".to_vec(),
            // Not UTF-8 late in the file: falls back to ANSI.
            [b"ascii start\r\n".repeat(40).as_slice(), &[b'x', 0xE9, b'\r', b'\n']].concat(),
        ];
        let mut seed = 77u64;
        for raw in samples {
            let whole = decode(raw.clone(), None);
            for _ in 0..60 {
                let mut d = StreamDecoder::new(None);
                let mut out = Vec::new();
                let mut i = 0;
                let mut restarted = false;
                while i < raw.len() {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    let n = 1 + (seed as usize % 9);
                    let end = (i + n).min(raw.len());
                    match d.push(&raw[i..end], end == raw.len()) {
                        Ok(t) => out.extend(t),
                        Err(NotUtf8) => {
                            // Start over as ANSI, as the loader does.
                            assert!(!restarted);
                            restarted = true;
                            d = StreamDecoder::new(Some(Enc::Ansi));
                            out.clear();
                            i = 0;
                            continue;
                        }
                    }
                    i = end;
                }
                let (enc, eol, mixed) = d.finish();
                assert_eq!(String::from_utf8_lossy(&out), String::from_utf8_lossy(&whole.text), "{:?}", String::from_utf8_lossy(&raw));
                assert_eq!((enc, eol, mixed), (whole.enc, whole.eol, whole.mixed_eol), "{:?}", String::from_utf8_lossy(&raw));
            }
        }
    }
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
