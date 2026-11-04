//! Document storage: one gap buffer over UTF-8 bytes plus an incrementally
//! maintained index of line starts. Line endings are normalised to `\n` on
//! load (see `enc.rs`); the original style is restored on save.

const GAP_MIN: usize = 1 << 16;

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

/// Calls `f` with the index of every `needle` byte, testing 8 bytes per step.
/// (The zero-byte trick can flag a byte next to a real hit, so each
/// candidate is checked.)
#[inline]
pub fn each_byte(hay: &[u8], needle: u8, mut f: impl FnMut(usize)) {
    let pat = LO.wrapping_mul(needle as u64);
    let mut chunks = hay.chunks_exact(8);
    let mut base = 0;
    for c in &mut chunks {
        let v = u64::from_le_bytes(c.try_into().unwrap()) ^ pat;
        let mut hit = v.wrapping_sub(LO) & !v & HI;
        while hit != 0 {
            let i = base + (hit.trailing_zeros() / 8) as usize;
            if hay[i] == needle {
                f(i);
            }
            hit &= hit - 1;
        }
        base += 8;
    }
    for (k, &b) in chunks.remainder().iter().enumerate() {
        if b == needle {
            f(base + k);
        }
    }
}

/// First index of `needle`, 8 bytes per step.
pub fn find_byte(hay: &[u8], needle: u8) -> Option<usize> {
    let pat = LO.wrapping_mul(needle as u64);
    let mut chunks = hay.chunks_exact(8);
    let mut base = 0;
    for c in &mut chunks {
        let v = u64::from_le_bytes(c.try_into().unwrap()) ^ pat;
        if v.wrapping_sub(LO) & !v & HI != 0 {
            if let Some(k) = c.iter().position(|&b| b == needle) {
                return Some(base + k);
            }
        }
        base += 8;
    }
    chunks.remainder().iter().position(|&b| b == needle).map(|k| base + k)
}

/// Gap reserved when a document of `len` bytes is loaded.
fn initial_gap(len: usize) -> usize {
    len.div_ceil(8).clamp(GAP_MIN, 1 << 20)
}

/// Reads a file into a buffer that already has room for the editing gap,
/// so loading a large file never reallocates (and briefly doubles) it.
pub fn read_for_doc(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata().map(|m| m.len() as usize).unwrap_or(0);
    let mut v = Vec::with_capacity(len + initial_gap(len) + 16);
    f.read_to_end(&mut v)?;
    Ok(v)
}

/// Raw bytes plus a movable insertion gap. One allocation for the whole document.
pub struct Gap {
    pub buf: Vec<u8>,
    pub gs: usize,
    pub ge: usize,
}

impl Gap {
    pub fn from_bytes(data: Vec<u8>) -> Self {
        let gap = initial_gap(data.len());
        let mut buf = data;
        let gs = buf.len();
        buf.reserve_exact(gap);
        buf.resize(gs + gap, 0u8);
        let ge = buf.len();
        Self { buf, gs, ge }
    }

    pub fn size(&self) -> usize {
        self.ge - self.gs
    }

    pub fn text_len(&self) -> usize {
        self.buf.len() - self.size()
    }

    pub fn to_phys(&self, l: usize) -> usize {
        if l >= self.gs {
            l + self.size()
        } else {
            l
        }
    }

    pub fn place_gap(&mut self, l: usize) {
        let gap = self.size();
        if l < self.gs {
            self.buf.copy_within(l..self.gs, l + gap);
            self.gs = l;
            self.ge = l + gap;
        } else if l > self.gs {
            let n = l - self.gs;
            self.buf.copy_within(self.ge..self.ge + n, self.gs);
            self.gs = l;
            self.ge = l + gap;
        }
    }

    pub fn insert(&mut self, l: usize, s: &[u8]) {
        self.place_gap(l);
        if s.len() <= self.size() {
            self.buf[self.gs..self.gs + s.len()].copy_from_slice(s);
            self.gs += s.len();
            return;
        }
        let grow = (self.text_len() / 8).clamp(GAP_MIN, 1 << 22);
        let mut out = Vec::with_capacity(self.text_len() + s.len() + grow);
        out.extend_from_slice(&self.buf[..self.gs]);
        out.extend_from_slice(s);
        let gs = out.len();
        out.resize(gs + grow, 0u8);
        let ge = out.len();
        out.extend_from_slice(&self.buf[self.ge..]);
        *self = Self { buf: out, gs, ge };
    }

    pub fn remove(&mut self, l: usize, n: usize) {
        if n == 0 {
            return;
        }
        self.place_gap(l);
        // The n bytes after the gap are abandoned into it, so the gap grows and
        // the surviving tail keeps its physical position.
        self.ge += n;
    }

    pub fn copy_range(&self, l: usize, r: usize, out: &mut Vec<u8>) {
        out.clear();
        if r <= l {
            return;
        }
        if r <= self.gs {
            out.extend_from_slice(&self.buf[l..r]);
        } else if l >= self.gs {
            out.extend_from_slice(&self.buf[self.to_phys(l)..self.to_phys(r)]);
        } else {
            out.extend_from_slice(&self.buf[l..self.gs]);
            out.extend_from_slice(&self.buf[self.ge..self.to_phys(r)]);
        }
    }

    pub fn byte_at(&self, l: usize) -> u8 {
        self.buf[self.to_phys(l)]
    }

    /// Moves the gap to the end so the whole text is one slice.
    pub fn contiguous(&mut self) -> &[u8] {
        let n = self.text_len();
        self.place_gap(n);
        &self.buf[..n]
    }
}

pub struct Doc {
    pub gap: Gap,
    pub starts: Vec<u32>,
}

impl Doc {
    pub fn load(data: Vec<u8>) -> Self {
        let gap = Gap::from_bytes(data);
        let starts = Self::scan(&gap);
        Self { gap, starts }
    }

    pub fn scan(gap: &Gap) -> Vec<u32> {
        let (a, b) = (&gap.buf[..gap.gs], &gap.buf[gap.ge..]);
        // Guess the line count from the first 64 KB so the index is sized once.
        let sample = &a[..a.len().min(1 << 16)];
        let mut n = 0usize;
        each_byte(sample, b'\n', |_| n += 1);
        let est = if sample.is_empty() { 1 } else { (gap.text_len() as u64 * (n as u64 + 1) / sample.len() as u64) as usize + 16 };
        let mut starts: Vec<u32> = Vec::with_capacity(est);
        starts.push(0);
        each_byte(a, b'\n', |i| starts.push((i + 1) as u32));
        let gs = gap.gs;
        each_byte(b, b'\n', |i| starts.push((gs + i + 1) as u32));
        starts
    }

    pub fn len(&self) -> usize {
        self.gap.text_len()
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    pub fn line_start(&self, i: usize) -> usize {
        self.starts[i] as usize
    }

    pub fn line_bounds(&self, i: usize) -> (usize, usize) {
        let s = self.starts[i] as usize;
        let e = if i + 1 < self.starts.len() {
            self.starts[i + 1] as usize
        } else {
            self.gap.text_len()
        };
        (s, e)
    }

    /// End of line `i` excluding its newline.
    pub fn line_end(&self, i: usize) -> usize {
        let (s, e) = self.line_bounds(i);
        if e > s && self.gap.byte_at(e - 1) == b'\n' {
            e - 1
        } else {
            e
        }
    }

    pub fn line_of(&self, pos: usize) -> usize {
        self.starts.partition_point(|&p| (p as usize) <= pos) - 1
    }

    pub fn line_bytes(&self, i: usize, out: &mut Vec<u8>) {
        self.gap.copy_range(self.line_start(i), self.line_end(i), out);
    }

    pub fn text(&self, l: usize, r: usize) -> Vec<u8> {
        let mut v = Vec::new();
        self.gap.copy_range(l, r, &mut v);
        v
    }

    pub fn byte(&self, p: usize) -> u8 {
        self.gap.byte_at(p)
    }

    /// Start of the UTF-8 character before `p`.
    pub fn prev_char(&self, p: usize) -> usize {
        let mut q = p;
        while q > 0 {
            q -= 1;
            if self.gap.byte_at(q) & 0xC0 != 0x80 {
                return q;
            }
        }
        0
    }

    /// Start of the UTF-8 character after `p`.
    pub fn next_char(&self, p: usize) -> usize {
        let n = self.len();
        let mut q = p + 1;
        while q < n && self.gap.byte_at(q) & 0xC0 == 0x80 {
            q += 1;
        }
        q.min(n)
    }

    /// Incremental index maintenance: only the edited line and the logical
    /// offsets after it move, so an edit never rescans the whole buffer.
    pub fn insert_at(&mut self, l: usize, s: &[u8]) {
        self.gap.insert(l, s);
        if s.is_empty() {
            return;
        }
        let delta = s.len() as u32;
        let idx = self.starts.partition_point(|&p| (p as usize) <= l);
        let fresh: Vec<u32> = s
            .iter()
            .enumerate()
            .filter(|(_, &b)| b == b'\n')
            .map(|(k, _)| (l + k + 1) as u32)
            .collect();
        for st in self.starts[idx..].iter_mut() {
            *st += delta;
        }
        if !fresh.is_empty() {
            self.starts.splice(idx..idx, fresh);
        }
    }

    pub fn remove_at(&mut self, l: usize, n: usize) {
        if n == 0 {
            return;
        }
        self.gap.remove(l, n);
        let end = (l + n) as u32;
        let idx = self.starts.partition_point(|&p| p <= (l as u32));
        let last = self.starts.partition_point(|&p| p <= end);
        self.starts.drain(idx..last);
        let delta = n as u32;
        for st in self.starts[idx..].iter_mut() {
            *st -= delta;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn sample(n: usize) -> Doc {
        let mut data = Vec::new();
        for i in 0..n {
            data.extend_from_slice(format!("line {i} -> some code with 空间 and tabs\t\n").as_bytes());
        }
        Doc::load(data)
    }

    fn logical(gap: &Gap) -> Vec<u8> {
        let mut out = Vec::new();
        gap.copy_range(0, gap.text_len(), &mut out);
        out
    }

    #[test]
    fn gap_matches_plain_vector_model() {
        let mut g = Gap::from_bytes(b"hello\nworld\n".to_vec());
        let mut m: Vec<u8> = b"hello\nworld\n".to_vec();
        let mut state = 0xCAFEBABEusize;
        for step in 0..6000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let big = g.text_len() + 1;
            let at = (state >> 33) % big;
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            if state >> 63 == 0 {
                let s = &[b'q', b'\n', b'r', b'\n'][..1 + (state >> 40) % 4];
                g.insert(at, s);
                m.splice(at..at, s.iter().copied());
            } else {
                let n = (1 + (state >> 40) % 5).min(g.text_len() - at);
                g.remove(at, n);
                m.drain(at..at + n);
            }
            assert_eq!(logical(&g), m, "gap diverged at step {step}");
        }
    }

    #[test]
    fn gap_grows_past_capacity() {
        let mut g = Gap::from_bytes(b"ab".to_vec());
        let big = vec![b'x'; GAP_MIN * 3];
        g.insert(1, &big);
        g.insert(0, b"<");
        let mut want = b"<a".to_vec();
        want.extend_from_slice(&big);
        want.push(b'b');
        assert_eq!(logical(&g), want);
        assert_eq!(g.contiguous(), &want[..]);
    }

    #[test]
    fn incremental_index_matches_full_scan() {
        let mut doc = sample(400);
        for (at, text) in [(120usize, "abc"), (5, "\n\n"), (900, "x"), (40, "  ")] {
            doc.insert_at(at, text.as_bytes());
            assert_eq!(doc.starts, Doc::scan(&doc.gap));
        }
        for (at, n) in [(121usize, 2usize), (5, 3), (0, 1), (880, 4), (889, 1)] {
            doc.remove_at(at, n);
            assert_eq!(doc.starts, Doc::scan(&doc.gap), "after remove {at}+{n}");
        }
    }

    #[test]
    fn random_edits_keep_index_line_starts_and_text_intact() {
        let mut doc = sample(2000);
        let mut state = 0x12345678usize;
        for step in 0..4000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let big = doc.gap.text_len().max(1);
            let at = (state >> 33) % big;
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            if state >> 63 == 0 {
                let n = 1 + (state >> 40) % 7;
                doc.insert_at(at, &[(b'a' + (n % 26) as u8), b'\n', b'z', b' ', 0xC3, 0xA9, b'\n'][..n.min(7)]);
            } else {
                let n = (1 + (state >> 40) % 5).min(doc.gap.text_len() - at);
                doc.remove_at(at, n);
            }
            assert_eq!(doc.starts, Doc::scan(&doc.gap), "step {step}");
        }
    }

    #[test]
    fn line_helpers() {
        let doc = Doc::load("ab\nc€\n\nz".as_bytes().to_vec());
        assert_eq!(doc.line_count(), 4);
        assert_eq!(doc.line_end(1), 7);
        assert_eq!(doc.line_of(3), 1);
        assert_eq!(doc.line_of(8), 2);
        assert_eq!(doc.prev_char(7), 4);
        assert_eq!(doc.next_char(4), 7);
    }

    /// Phase timings for loading a large file named by SLATE_HUGE.
    #[test]
    fn huge_load_phases() {
        let Some(p) = std::env::var_os("SLATE_HUGE") else { return };
        let t = Instant::now();
        let raw = read_for_doc(std::path::Path::new(&p)).unwrap();
        let read = t.elapsed();
        let t = Instant::now();
        let enc = crate::enc::detect(&raw);
        let detect = t.elapsed();
        let t = Instant::now();
        let d = crate::enc::decode(raw, Some(enc));
        let decode = t.elapsed();
        let t = Instant::now();
        let doc = Doc::load(d.text);
        let index = t.elapsed();
        println!(
            "bytes={} lines={} read={:?} detect={:?} decode+eol={:?} gap+index={:?}",
            doc.len(),
            doc.line_count(),
            read,
            detect,
            decode,
            index
        );
    }

    #[test]
    fn edit_latency_on_10mb_doc() {
        let mut doc = sample(200_000);
        let mid = doc.gap.text_len() / 2;
        let t = Instant::now();
        for i in 0..2000 {
            doc.insert_at(mid + i, b"x");
        }
        let ins = t.elapsed();
        let t = Instant::now();
        for _ in 0..2000 {
            doc.remove_at(mid, 1);
        }
        let del = t.elapsed();
        println!(
            "lines={} bytes={} insert={:.1} us/edit remove={:.1} us/edit",
            doc.line_count(),
            doc.gap.text_len(),
            ins.as_secs_f64() * 500.0,
            del.as_secs_f64() * 500.0
        );
    }
}
