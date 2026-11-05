//! Messages Slate instances exchange over the LAN, and their compact
//! binary encoding. Every message travels in a frame: a little-endian u32
//! length, then the encoded message.

/// Bumped when messages change incompatibly; peers with another version
/// are ignored rather than misread.
pub const PROTOCOL: u32 = 1;
/// Largest frame accepted (a co-editing snapshot of a big file).
pub const MAX_FRAME: usize = 256 << 20;

#[derive(Clone, Debug, PartialEq)]
pub struct PresenceFile {
    pub path: String,
    /// 0 open, 1 unsaved changes, 2 locked.
    pub state: u8,
    pub since: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    pub sid: u64,
    /// Session lineage: a new one each time a session is started.
    pub gen: u64,
    pub host: u64,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LibMeta {
    /// 0 snippet, 1 macro, 2 preset.
    pub kind: u8,
    pub name: String,
    /// Milliseconds since 1970; newest wins.
    pub mtime: u64,
    pub hash: u64,
    pub deleted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Hello { id: u64, protocol: u32, name: String, pc: String, version: String, hub: bool },
    Presence { files: Vec<PresenceFile>, sessions: Vec<SessionInfo> },
    // Co-editing. `op` is an ot::Op in its encoded form.
    Join { sid: u64 },
    Snapshot { sid: u64, gen: u64, rev: u64, host: u64, members: Vec<u64>, text: Vec<u8> },
    Op { sid: u64, gen: u64, seq: u64, base: u64, op: Vec<u8> },
    Seq { sid: u64, gen: u64, rev: u64, author: u64, seq: u64, op: Vec<u8> },
    Members { sid: u64, gen: u64, rev: u64, host: u64, members: Vec<u64> },
    Cursor { sid: u64, rev: u64, anchor: u64, head: u64 },
    Saved { sid: u64, gen: u64, rev: u64 },
    Leave { sid: u64 },
    Elect { sid: u64, gen: u64, rev: u64 },
    Resume { sid: u64, gen: u64, rev: u64 },
    ResumeDone { sid: u64, gen: u64, rev: u64 },
    Refuse { sid: u64, reason: String },
    /// Answer to `Elect` from someone who still reaches the host.
    Alive { sid: u64, gen: u64, host: u64 },
    // Team library.
    LibManifest { items: Vec<LibMeta> },
    LibWant { items: Vec<(u8, String)> },
    LibItem { meta: LibMeta, body: Vec<u8> },
    // Instant search through a hub.
    SearchReq { qid: u64, query: String },
    SearchRes { qid: u64, text: String },
}

struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn ids(&mut self, v: &[u64]) {
        self.u32(v.len() as u32);
        for &x in v {
            self.u64(x);
        }
    }
    fn meta(&mut self, m: &LibMeta) {
        self.u8(m.kind);
        self.str(&m.name);
        self.u64(m.mtime);
        self.u64(m.hash);
        self.u8(m.deleted as u8);
    }
}

struct R<'a>(&'a [u8], usize);

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.0.get(self.1..self.1.checked_add(n)?)?;
        self.1 += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn bytes(&mut self) -> Option<Vec<u8>> {
        let n = self.u32()? as usize;
        Some(self.take(n)?.to_vec())
    }
    fn str(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?).ok()
    }
    /// A count, checked against what is left so garbage cannot ask for a
    /// huge allocation.
    fn count(&mut self, min_item: usize) -> Option<usize> {
        let n = self.u32()? as usize;
        (n.checked_mul(min_item)? <= self.0.len() - self.1).then_some(n)
    }
    fn ids(&mut self) -> Option<Vec<u64>> {
        let n = self.count(8)?;
        (0..n).map(|_| self.u64()).collect()
    }
    fn meta(&mut self) -> Option<LibMeta> {
        Some(LibMeta { kind: self.u8()?, name: self.str()?, mtime: self.u64()?, hash: self.u64()?, deleted: self.u8()? != 0 })
    }
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W(Vec::with_capacity(64));
        match self {
            Msg::Hello { id, protocol, name, pc, version, hub } => {
                w.u8(1);
                w.u64(*id);
                w.u32(*protocol);
                w.str(name);
                w.str(pc);
                w.str(version);
                w.u8(*hub as u8);
            }
            Msg::Presence { files, sessions } => {
                w.u8(2);
                w.u32(files.len() as u32);
                for f in files {
                    w.str(&f.path);
                    w.u8(f.state);
                    w.u64(f.since);
                }
                w.u32(sessions.len() as u32);
                for s in sessions {
                    w.u64(s.sid);
                    w.u64(s.gen);
                    w.u64(s.host);
                    w.str(&s.path);
                }
            }
            Msg::Join { sid } => {
                w.u8(10);
                w.u64(*sid);
            }
            Msg::Snapshot { sid, gen, rev, host, members, text } => {
                w.u8(11);
                w.u64(*sid);
                w.u64(*gen);
                w.u64(*rev);
                w.u64(*host);
                w.ids(members);
                w.bytes(text);
            }
            Msg::Op { sid, gen, seq, base, op } => {
                w.u8(12);
                w.u64(*sid);
                w.u64(*gen);
                w.u64(*seq);
                w.u64(*base);
                w.bytes(op);
            }
            Msg::Seq { sid, gen, rev, author, seq, op } => {
                w.u8(13);
                w.u64(*sid);
                w.u64(*gen);
                w.u64(*rev);
                w.u64(*author);
                w.u64(*seq);
                w.bytes(op);
            }
            Msg::Members { sid, gen, rev, host, members } => {
                w.u8(14);
                w.u64(*sid);
                w.u64(*gen);
                w.u64(*rev);
                w.u64(*host);
                w.ids(members);
            }
            Msg::Cursor { sid, rev, anchor, head } => {
                w.u8(15);
                w.u64(*sid);
                w.u64(*rev);
                w.u64(*anchor);
                w.u64(*head);
            }
            Msg::Saved { sid, gen, rev } => {
                w.u8(16);
                w.u64(*sid);
                w.u64(*gen);
                w.u64(*rev);
            }
            Msg::Leave { sid } => {
                w.u8(17);
                w.u64(*sid);
            }
            Msg::Elect { sid, gen, rev } => {
                w.u8(18);
                w.u64(*sid);
                w.u64(*gen);
                w.u64(*rev);
            }
            Msg::Resume { sid, gen, rev } => {
                w.u8(19);
                w.u64(*sid);
                w.u64(*gen);
                w.u64(*rev);
            }
            Msg::ResumeDone { sid, gen, rev } => {
                w.u8(20);
                w.u64(*sid);
                w.u64(*gen);
                w.u64(*rev);
            }
            Msg::Refuse { sid, reason } => {
                w.u8(21);
                w.u64(*sid);
                w.str(reason);
            }
            Msg::Alive { sid, gen, host } => {
                w.u8(22);
                w.u64(*sid);
                w.u64(*gen);
                w.u64(*host);
            }
            Msg::LibManifest { items } => {
                w.u8(30);
                w.u32(items.len() as u32);
                for m in items {
                    w.meta(m);
                }
            }
            Msg::LibWant { items } => {
                w.u8(31);
                w.u32(items.len() as u32);
                for (k, n) in items {
                    w.u8(*k);
                    w.str(n);
                }
            }
            Msg::LibItem { meta, body } => {
                w.u8(32);
                w.meta(meta);
                w.bytes(body);
            }
            Msg::SearchReq { qid, query } => {
                w.u8(40);
                w.u64(*qid);
                w.str(query);
            }
            Msg::SearchRes { qid, text } => {
                w.u8(41);
                w.u64(*qid);
                w.str(text);
            }
        }
        w.0
    }

    pub fn decode(b: &[u8]) -> Option<Msg> {
        let mut r = R(b, 0);
        let m = match r.u8()? {
            1 => Msg::Hello { id: r.u64()?, protocol: r.u32()?, name: r.str()?, pc: r.str()?, version: r.str()?, hub: r.u8()? != 0 },
            2 => {
                let n = r.count(13)?;
                let files = (0..n).map(|_| Some(PresenceFile { path: r.str()?, state: r.u8()?, since: r.u64()? })).collect::<Option<Vec<_>>>()?;
                let n = r.count(28)?;
                let sessions = (0..n).map(|_| Some(SessionInfo { sid: r.u64()?, gen: r.u64()?, host: r.u64()?, path: r.str()? })).collect::<Option<Vec<_>>>()?;
                Msg::Presence { files, sessions }
            }
            10 => Msg::Join { sid: r.u64()? },
            11 => Msg::Snapshot { sid: r.u64()?, gen: r.u64()?, rev: r.u64()?, host: r.u64()?, members: r.ids()?, text: r.bytes()? },
            12 => Msg::Op { sid: r.u64()?, gen: r.u64()?, seq: r.u64()?, base: r.u64()?, op: r.bytes()? },
            13 => Msg::Seq { sid: r.u64()?, gen: r.u64()?, rev: r.u64()?, author: r.u64()?, seq: r.u64()?, op: r.bytes()? },
            14 => Msg::Members { sid: r.u64()?, gen: r.u64()?, rev: r.u64()?, host: r.u64()?, members: r.ids()? },
            15 => Msg::Cursor { sid: r.u64()?, rev: r.u64()?, anchor: r.u64()?, head: r.u64()? },
            16 => Msg::Saved { sid: r.u64()?, gen: r.u64()?, rev: r.u64()? },
            17 => Msg::Leave { sid: r.u64()? },
            18 => Msg::Elect { sid: r.u64()?, gen: r.u64()?, rev: r.u64()? },
            19 => Msg::Resume { sid: r.u64()?, gen: r.u64()?, rev: r.u64()? },
            20 => Msg::ResumeDone { sid: r.u64()?, gen: r.u64()?, rev: r.u64()? },
            21 => Msg::Refuse { sid: r.u64()?, reason: r.str()? },
            22 => Msg::Alive { sid: r.u64()?, gen: r.u64()?, host: r.u64()? },
            30 => {
                let n = r.count(26)?;
                Msg::LibManifest { items: (0..n).map(|_| r.meta()).collect::<Option<Vec<_>>>()? }
            }
            31 => {
                let n = r.count(5)?;
                Msg::LibWant { items: (0..n).map(|_| Some((r.u8()?, r.str()?))).collect::<Option<Vec<_>>>()? }
            }
            32 => Msg::LibItem { meta: r.meta()?, body: r.bytes()? },
            40 => Msg::SearchReq { qid: r.u64()?, query: r.str()? },
            41 => Msg::SearchRes { qid: r.u64()?, text: r.str()? },
            _ => return None,
        };
        // Trailing bytes mean a malformed or foreign message.
        (r.1 == b.len()).then_some(m)
    }

    /// The message in a length-prefixed frame.
    pub fn frame(&self) -> Vec<u8> {
        let body = self.encode();
        let mut out = Vec::with_capacity(body.len() + 4);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }
}

/// UDP discovery datagram: who is here and where to connect.
#[derive(Clone, Debug, PartialEq)]
pub struct Beacon {
    pub id: u64,
    pub port: u16,
    /// A query asks everyone who hears it to answer straight back.
    pub query: bool,
}

const BEACON_MAGIC: &[u8; 6] = b"SLATE1";

impl Beacon {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = BEACON_MAGIC.to_vec();
        v.extend_from_slice(&PROTOCOL.to_le_bytes());
        v.extend_from_slice(&self.id.to_le_bytes());
        v.extend_from_slice(&self.port.to_le_bytes());
        v.push(self.query as u8);
        v
    }

    pub fn decode(b: &[u8]) -> Option<Beacon> {
        if b.len() != 21 || &b[..6] != BEACON_MAGIC || u32::from_le_bytes(b[6..10].try_into().ok()?) != PROTOCOL {
            return None;
        }
        Some(Beacon { id: u64::from_le_bytes(b[10..18].try_into().ok()?), port: u16::from_le_bytes(b[18..20].try_into().ok()?), query: b[20] != 0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_round_trips() {
        let meta = LibMeta { kind: 1, name: "Renumber spools".into(), mtime: 9, hash: 77, deleted: false };
        let all = vec![
            Msg::Hello { id: 1, protocol: PROTOCOL, name: "Ravi Kumar".into(), pc: "DDS-07".into(), version: "1.5.0".into(), hub: false },
            Msg::Presence {
                files: vec![PresenceFile { path: r"\\srv\p\a.pcf".into(), state: 1, since: 5 }],
                sessions: vec![SessionInfo { sid: 3, gen: 4, host: 1, path: r"\\srv\p\a.pcf".into() }],
            },
            Msg::Join { sid: 3 },
            Msg::Snapshot { sid: 3, gen: 4, rev: 10, host: 1, members: vec![1, 2], text: b"PIPE\n".to_vec() },
            Msg::Op { sid: 3, gen: 4, seq: 2, base: 9, op: b"r5 i1:x".to_vec() },
            Msg::Seq { sid: 3, gen: 4, rev: 11, author: 2, seq: 2, op: b"r5 i1:x".to_vec() },
            Msg::Members { sid: 3, gen: 4, rev: 11, host: 1, members: vec![1, 2, 5] },
            Msg::Cursor { sid: 3, rev: 11, anchor: 1, head: 4 },
            Msg::Saved { sid: 3, gen: 4, rev: 11 },
            Msg::Leave { sid: 3 },
            Msg::Elect { sid: 3, gen: 4, rev: 11 },
            Msg::Resume { sid: 3, gen: 4, rev: 8 },
            Msg::ResumeDone { sid: 3, gen: 4, rev: 11 },
            Msg::Refuse { sid: 3, reason: "no".into() },
            Msg::Alive { sid: 3, gen: 4, host: 1 },
            Msg::LibManifest { items: vec![meta.clone()] },
            Msg::LibWant { items: vec![(1, "Renumber spools".into())] },
            Msg::LibItem { meta, body: b"k 36 1 0 0\n".to_vec() },
            Msg::SearchReq { qid: 8, query: "FW1A06".into() },
            Msg::SearchRes { qid: 8, text: "Search ...".into() },
        ];
        for m in all {
            assert_eq!(Msg::decode(&m.encode()), Some(m.clone()), "{m:?}");
        }
        let b = Beacon { id: 42, port: 50123, query: true };
        assert_eq!(Beacon::decode(&b.encode()), Some(b));
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(Msg::decode(&[]), None);
        assert_eq!(Msg::decode(&[99, 1, 2]), None);
        // A huge count must not allocate.
        assert_eq!(Msg::decode(&[30, 255, 255, 255, 255]), None);
        let mut m = Msg::Join { sid: 1 }.encode();
        m.push(0);
        assert_eq!(Msg::decode(&m), None);
        assert_eq!(Beacon::decode(b"SLATE1xxxx"), None);
    }
}
