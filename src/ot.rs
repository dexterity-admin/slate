//! Operational transformation on byte text, for co-editing: an operation
//! retains, inserts and deletes bytes over the whole document; operations
//! can be composed and transformed against each other (the ot.js model).

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    Retain(usize),
    Insert(Vec<u8>),
    Delete(usize),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Op {
    pub parts: Vec<Part>,
    /// Document length the operation applies to, and the length after.
    pub base: usize,
    pub target: usize,
}

impl Op {
    pub fn new() -> Op {
        Op::default()
    }

    pub fn retain(&mut self, n: usize) -> &mut Self {
        if n == 0 {
            return self;
        }
        self.base += n;
        self.target += n;
        if let Some(Part::Retain(k)) = self.parts.last_mut() {
            *k += n;
        } else {
            self.parts.push(Part::Retain(n));
        }
        self
    }

    pub fn insert(&mut self, s: &[u8]) -> &mut Self {
        if s.is_empty() {
            return self;
        }
        self.target += s.len();
        let n = self.parts.len();
        // Inserts go before an adjacent delete, so equal operations have one
        // canonical form.
        match self.parts.last_mut() {
            Some(Part::Insert(t)) => t.extend_from_slice(s),
            Some(Part::Delete(_)) => {
                if n >= 2 {
                    if let Part::Insert(t) = &mut self.parts[n - 2] {
                        t.extend_from_slice(s);
                        return self;
                    }
                }
                self.parts.insert(n - 1, Part::Insert(s.to_vec()));
            }
            _ => self.parts.push(Part::Insert(s.to_vec())),
        }
        self
    }

    pub fn delete(&mut self, n: usize) -> &mut Self {
        if n == 0 {
            return self;
        }
        self.base += n;
        if let Some(Part::Delete(k)) = self.parts.last_mut() {
            *k += n;
        } else {
            self.parts.push(Part::Delete(n));
        }
        self
    }

    /// Replacing bytes a..b of a document of length `len` with `text`.
    pub fn replace(len: usize, a: usize, b: usize, text: &[u8]) -> Op {
        let mut o = Op::new();
        o.retain(a).delete(b - a).insert(text).retain(len - b);
        o
    }

    /// The operation as `(start, end, text)` changes in the coordinates of
    /// the document before it, ascending.
    pub fn changes(&self) -> Vec<(usize, usize, Vec<u8>)> {
        let mut out: Vec<(usize, usize, Vec<u8>)> = Vec::new();
        let mut at = 0;
        for p in &self.parts {
            match p {
                Part::Retain(n) => at += n,
                Part::Insert(s) => match out.last_mut() {
                    Some(c) if c.1 == at => c.2.extend_from_slice(s),
                    _ => out.push((at, at, s.clone())),
                },
                Part::Delete(n) => {
                    match out.last_mut() {
                        Some(c) if c.1 == at => c.1 += n,
                        _ => out.push((at, at + n, Vec::new())),
                    }
                    at += n;
                }
            }
        }
        out
    }

    pub fn apply(&self, doc: &[u8]) -> Option<Vec<u8>> {
        if doc.len() != self.base {
            return None;
        }
        let mut out = Vec::with_capacity(self.target);
        let mut at = 0;
        for p in &self.parts {
            match p {
                Part::Retain(n) => {
                    out.extend_from_slice(&doc[at..at + n]);
                    at += n;
                }
                Part::Insert(s) => out.extend_from_slice(s),
                Part::Delete(n) => at += n,
            }
        }
        Some(out)
    }

    /// `self` followed by `b` as one operation.
    pub fn compose(&self, b: &Op) -> Option<Op> {
        if self.target != b.base {
            return None;
        }
        let mut res = Op::new();
        let (mut i1, mut i2) = (self.parts.iter().cloned(), b.parts.iter().cloned());
        let (mut o1, mut o2) = (i1.next(), i2.next());
        loop {
            match (&o1, &o2) {
                (None, None) => break,
                (Some(Part::Delete(n)), _) => {
                    res.delete(*n);
                    o1 = i1.next();
                    continue;
                }
                (_, Some(Part::Insert(s))) => {
                    res.insert(s);
                    o2 = i2.next();
                    continue;
                }
                (None, _) | (_, None) => return None,
                _ => {}
            }
            match (o1.take().unwrap(), o2.take().unwrap()) {
                (Part::Retain(n1), Part::Retain(n2)) => {
                    let m = n1.min(n2);
                    res.retain(m);
                    o1 = if n1 > m { Some(Part::Retain(n1 - m)) } else { i1.next() };
                    o2 = if n2 > m { Some(Part::Retain(n2 - m)) } else { i2.next() };
                }
                (Part::Insert(s), Part::Delete(n)) => {
                    let m = s.len().min(n);
                    o1 = if s.len() > m { Some(Part::Insert(s[m..].to_vec())) } else { i1.next() };
                    o2 = if n > m { Some(Part::Delete(n - m)) } else { i2.next() };
                }
                (Part::Insert(s), Part::Retain(n)) => {
                    let m = s.len().min(n);
                    res.insert(&s[..m]);
                    o1 = if s.len() > m { Some(Part::Insert(s[m..].to_vec())) } else { i1.next() };
                    o2 = if n > m { Some(Part::Retain(n - m)) } else { i2.next() };
                }
                (Part::Retain(n1), Part::Delete(n2)) => {
                    let m = n1.min(n2);
                    res.delete(m);
                    o1 = if n1 > m { Some(Part::Retain(n1 - m)) } else { i1.next() };
                    o2 = if n2 > m { Some(Part::Delete(n2 - m)) } else { i2.next() };
                }
                _ => unreachable!(),
            }
        }
        Some(res)
    }

    /// For concurrent `a` and `b` on the same document, `(a', b')` such
    /// that a then b' equals b then a'. Inserts of `a` go first at a tie.
    pub fn transform(a: &Op, b: &Op) -> Option<(Op, Op)> {
        if a.base != b.base {
            return None;
        }
        let (mut a2, mut b2) = (Op::new(), Op::new());
        let (mut i1, mut i2) = (a.parts.iter().cloned(), b.parts.iter().cloned());
        let (mut o1, mut o2) = (i1.next(), i2.next());
        loop {
            match (&o1, &o2) {
                (None, None) => break,
                (Some(Part::Insert(s)), _) => {
                    a2.insert(s);
                    b2.retain(s.len());
                    o1 = i1.next();
                    continue;
                }
                (_, Some(Part::Insert(s))) => {
                    a2.retain(s.len());
                    b2.insert(s);
                    o2 = i2.next();
                    continue;
                }
                (None, _) | (_, None) => return None,
                _ => {}
            }
            match (o1.take().unwrap(), o2.take().unwrap()) {
                (Part::Retain(n1), Part::Retain(n2)) => {
                    let m = n1.min(n2);
                    a2.retain(m);
                    b2.retain(m);
                    o1 = if n1 > m { Some(Part::Retain(n1 - m)) } else { i1.next() };
                    o2 = if n2 > m { Some(Part::Retain(n2 - m)) } else { i2.next() };
                }
                (Part::Delete(n1), Part::Delete(n2)) => {
                    let m = n1.min(n2);
                    o1 = if n1 > m { Some(Part::Delete(n1 - m)) } else { i1.next() };
                    o2 = if n2 > m { Some(Part::Delete(n2 - m)) } else { i2.next() };
                }
                (Part::Delete(n1), Part::Retain(n2)) => {
                    let m = n1.min(n2);
                    a2.delete(m);
                    o1 = if n1 > m { Some(Part::Delete(n1 - m)) } else { i1.next() };
                    o2 = if n2 > m { Some(Part::Retain(n2 - m)) } else { i2.next() };
                }
                (Part::Retain(n1), Part::Delete(n2)) => {
                    let m = n1.min(n2);
                    b2.delete(m);
                    o1 = if n1 > m { Some(Part::Retain(n1 - m)) } else { i1.next() };
                    o2 = if n2 > m { Some(Part::Delete(n2 - m)) } else { i2.next() };
                }
                _ => unreachable!(),
            }
        }
        Some((a2, b2))
    }

    /// Where position `p` ends up after this operation. Text inserted
    /// exactly at `p` by someone else goes before it when `after` is set.
    pub fn map_pos(&self, p: usize, after: bool) -> usize {
        let (mut at, mut out) = (0usize, p);
        for part in &self.parts {
            if at > p || (at == p && !after) {
                break;
            }
            match part {
                Part::Retain(n) => at += n,
                Part::Insert(s) => out += s.len(),
                Part::Delete(n) => {
                    out -= (*n).min(p.saturating_sub(at));
                    at += n;
                }
            }
        }
        out
    }

    /// Compact binary-safe form: `r12 d4 i5:hello`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (k, p) in self.parts.iter().enumerate() {
            if k > 0 {
                out.push(b' ');
            }
            match p {
                Part::Retain(n) => out.extend_from_slice(format!("r{n}").as_bytes()),
                Part::Delete(n) => out.extend_from_slice(format!("d{n}").as_bytes()),
                Part::Insert(s) => {
                    out.extend_from_slice(format!("i{}:", s.len()).as_bytes());
                    out.extend_from_slice(s);
                }
            }
        }
        out
    }

    pub fn decode(b: &[u8]) -> Option<Op> {
        let mut o = Op::new();
        let mut i = 0;
        let num = |i: &mut usize| -> Option<usize> {
            let s = *i;
            while *i < b.len() && b[*i].is_ascii_digit() {
                *i += 1;
            }
            std::str::from_utf8(&b[s..*i]).ok()?.parse().ok()
        };
        while i < b.len() {
            let k = b[i];
            i += 1;
            match k {
                b' ' => continue,
                b'r' => {
                    o.retain(num(&mut i)?);
                }
                b'd' => {
                    o.delete(num(&mut i)?);
                }
                b'i' => {
                    let n = num(&mut i)?;
                    if b.get(i) != Some(&b':') || i + 1 + n > b.len() {
                        return None;
                    }
                    o.insert(&b[i + 1..i + 1 + n]);
                    i += 1 + n;
                }
                _ => return None,
            }
        }
        Some(o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rnd(seed: &mut u64) -> usize {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 8) as usize
    }

    fn random_op(doc: &[u8], seed: &mut u64) -> Op {
        let mut o = Op::new();
        let mut at = 0;
        while at < doc.len() {
            let n = 1 + rnd(seed) % (doc.len() - at).min(5);
            match rnd(seed) % 4 {
                0 => {
                    o.retain(n);
                }
                1 => {
                    o.delete(n);
                }
                2 => {
                    o.insert(&b"xyz\n"[..1 + rnd(seed) % 4]);
                    continue;
                }
                _ => {
                    o.retain(n);
                }
            }
            at += n;
        }
        if rnd(seed) % 2 == 0 {
            o.insert(b"END");
        }
        o
    }

    #[test]
    fn apply_compose_transform_converge() {
        let mut seed = 0x9E3779B97F4A7C15u64;
        for _ in 0..500 {
            let doc: Vec<u8> = (0..rnd(&mut seed) % 30).map(|k| b'a' + (k % 26) as u8).collect();
            let a = random_op(&doc, &mut seed);
            let b = random_op(&doc, &mut seed);
            let (a2, b2) = Op::transform(&a, &b).unwrap();
            let ab = b2.apply(&a.apply(&doc).unwrap()).unwrap();
            let ba = a2.apply(&b.apply(&doc).unwrap()).unwrap();
            assert_eq!(ab, ba);
            let ad = a.apply(&doc).unwrap();
            let c = random_op(&ad, &mut seed);
            let ac = a.compose(&c).unwrap();
            assert_eq!(ac.apply(&doc).unwrap(), c.apply(&ad).unwrap());
            assert_eq!(Op::decode(&a.encode()).unwrap(), a);
            // Change lists reproduce the operation.
            let mut d = doc.clone();
            for (s, e, t) in a.changes().into_iter().rev() {
                d.splice(s..e, t);
            }
            assert_eq!(d, ad);
        }
    }

    #[test]
    fn insert_tie_and_positions() {
        let a = Op::replace(4, 2, 2, b"A");
        let b = Op::replace(4, 2, 2, b"B");
        let (a2, b2) = Op::transform(&a, &b).unwrap();
        assert_eq!(b2.apply(&a.apply(b"0123").unwrap()).unwrap(), b"01AB23");
        assert_eq!(a2.apply(&b.apply(b"0123").unwrap()).unwrap(), b"01AB23");
        let o = Op::replace(10, 2, 5, b"xy");
        assert_eq!(o.map_pos(1, false), 1);
        // Inside the replaced text: after the replacement.
        assert_eq!(o.map_pos(3, false), 4);
        assert_eq!(o.map_pos(8, false), 7);
        assert_eq!(Op::decode(b"r2 i3:a b d1").unwrap().apply(b"xyz").unwrap(), b"xya b");
    }
}
