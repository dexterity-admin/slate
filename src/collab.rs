//! Live co-editing over direct LAN connections.
//!
//! One participant, the host, puts every change in order (the ot.js
//! server); everyone else is an ot.js client: a client sends one change at
//! a time and transforms its unconfirmed changes over what the host sends.
//! Every participant keeps the ordered history, so when the host leaves or
//! its PC drops off, the others elect the one that saw the most changes,
//! catch up from it and resend whatever was not confirmed. Changes carry
//! per-author sequence numbers, so a resent change is never applied twice.
//!
//! `Core` is the session logic with no I/O, so it can be tested with a
//! simulated network; the `App` part below wires it to buffers and peers.

use crate::app::App;
use crate::ot::Op;
use crate::team;
use crate::wire::{Msg, SessionInfo};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Wait this long for a dropped host to come back before electing a new one.
const GRACE: Duration = Duration::from_millis(2000);
/// Votes from members that never answer stop counting after this.
const VOTE_WAIT: Duration = Duration::from_millis(3500);
/// Joining or catching up gives up after this.
const REPLY_WAIT: Duration = Duration::from_secs(8);
/// While someone still reaches the old host, wait this long for them to
/// notice it is gone too before giving up on the session.
const ALIVE_WAIT: Duration = Duration::from_secs(20);
/// Timer that drives elections and timeouts while one is under way.
pub const T_COLLAB: usize = 11;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Asked the host for the text; edits wait.
    Joining,
    Live,
    /// The host is gone; choosing a new one.
    Electing,
    /// Catching up from a new host.
    Resuming,
}

/// What the session asks its owner to do.
#[derive(Debug, PartialEq)]
pub enum Out {
    Send(u64, Msg),
    /// Send `Msg::Snapshot` with the current text to this peer.
    SendSnapshot(u64),
    /// Apply a co-editor's change to the local text.
    Apply(Op),
    /// Replace the local text (joining, or resynchronising).
    Install(Vec<u8>),
    MarkSaved,
    Note(String),
    /// The session is over for this buffer.
    Stop(String),
}

pub struct Core {
    pub me: u64,
    pub sid: u64,
    pub gen: u64,
    pub host: u64,
    /// Everyone in the session, this Slate included.
    pub members: Vec<u64>,
    /// Changes in the agreed order so far.
    pub rev: u64,
    hist_base: u64,
    /// (author, sequence, change) for revisions hist_base+1 ..= rev.
    history: Vec<(u64, u64, Op)>,
    last_seq: HashMap<u64, u64>,
    pub awaiting: Option<(u64, Op)>,
    pub pending: Option<Op>,
    next_seq: u64,
    pub phase: Phase,
    since: Instant,
    old_host: u64,
    grace: bool,
    voted: bool,
    /// Election: each member's revision, or None for "I still reach the host".
    votes: HashMap<u64, Option<u64>>,
    /// Catch-up requests that arrived before this Slate became host.
    queued: Vec<(u64, u64)>,
    /// Others' selections: (revision, anchor, head).
    pub cursors: HashMap<u64, (u64, u64, u64)>,
    last_cursor: Option<(u64, u64, u64)>,
}

impl Core {
    fn new(me: u64, sid: u64, gen: u64, host: u64, phase: Phase) -> Core {
        Core {
            me,
            sid,
            gen,
            host,
            members: vec![me],
            rev: 0,
            hist_base: 0,
            history: Vec::new(),
            last_seq: HashMap::new(),
            awaiting: None,
            pending: None,
            next_seq: 1,
            phase,
            since: Instant::now(),
            old_host: 0,
            grace: false,
            voted: false,
            votes: HashMap::new(),
            queued: Vec::new(),
            cursors: HashMap::new(),
            last_cursor: None,
        }
    }

    /// A new session with this Slate as host.
    pub fn host(me: u64, sid: u64, gen: u64) -> Core {
        Core::new(me, sid, gen, me, Phase::Live)
    }

    /// Joining the session `host` runs.
    pub fn join(me: u64, sid: u64, host: u64) -> (Core, Vec<Out>) {
        let mut c = Core::new(me, sid, 0, host, Phase::Joining);
        c.members.push(host);
        (c, vec![Out::Send(host, Msg::Join { sid })])
    }

    pub fn is_host(&self) -> bool {
        self.host == self.me && self.phase == Phase::Live
    }

    fn others(&self) -> Vec<u64> {
        self.members.iter().copied().filter(|&m| m != self.me).collect()
    }

    fn to_others(&self, m: Msg) -> Vec<Out> {
        self.others().into_iter().map(|id| Out::Send(id, m.clone())).collect()
    }

    fn set_phase(&mut self, p: Phase) {
        self.phase = p;
        self.since = Instant::now();
    }

    fn push(&mut self, author: u64, seq: u64, op: Op) {
        self.rev += 1;
        self.history.push((author, seq, op));
        let e = self.last_seq.entry(author).or_insert(0);
        *e = (*e).max(seq);
    }

    fn members_msg(&self) -> Msg {
        Msg::Members { sid: self.sid, gen: self.gen, rev: self.rev, host: self.host, members: self.members.clone() }
    }

    /// Whether the local text equals the agreed text at `rev`.
    pub fn in_step(&self) -> bool {
        self.awaiting.is_none() && self.pending.is_none()
    }

    /// A local edit.
    pub fn local(&mut self, op: Op) -> Vec<Out> {
        if self.is_host() {
            let seq = self.next_seq;
            self.next_seq += 1;
            self.push(self.me, seq, op.clone());
            let m = Msg::Seq { sid: self.sid, gen: self.gen, rev: self.rev, author: self.me, seq, op: op.encode() };
            return self.to_others(m);
        }
        self.pending = Some(match self.pending.take() {
            Some(p) => p.compose(&op).unwrap_or(op),
            None => op,
        });
        self.try_send()
    }

    /// Sends the next unconfirmed change when nothing is in flight.
    fn try_send(&mut self) -> Vec<Out> {
        if self.phase != Phase::Live || self.awaiting.is_some() || self.host == self.me {
            return Vec::new();
        }
        let Some(op) = self.pending.take() else { return Vec::new() };
        let seq = self.next_seq;
        self.next_seq += 1;
        let m = Msg::Op { sid: self.sid, gen: self.gen, seq, base: self.rev, op: op.encode() };
        self.awaiting = Some((seq, op));
        vec![Out::Send(self.host, m)]
    }

    /// An ordered change arrived (as a client).
    fn on_seq(&mut self, rev: u64, author: u64, seq: u64, op: Op) -> Vec<Out> {
        if rev <= self.rev {
            return Vec::new();
        }
        if rev != self.rev + 1 {
            // Something went missing: catch up from the host.
            return vec![Out::Send(self.host, Msg::Resume { sid: self.sid, gen: self.gen, rev: self.rev })];
        }
        self.push(author, seq, op.clone());
        if author == self.me {
            // Our own change came back: confirmed (it is already in the text).
            if self.awaiting.as_ref().map(|a| a.0) == Some(seq) {
                self.awaiting = None;
            }
            return self.try_send();
        }
        let mut x = op;
        for slot in [&mut self.awaiting.as_mut().map(|a| &mut a.1), &mut self.pending.as_mut()] {
            if let Some(o) = slot.as_deref_mut() {
                if let Some((o2, x2)) = Op::transform(o, &x) {
                    *o = o2;
                    x = x2;
                }
            }
        }
        vec![Out::Apply(x)]
    }

    /// A client's change (as host).
    fn on_op(&mut self, from: u64, seq: u64, base: u64, op: Op) -> Vec<Out> {
        let mut out = Vec::new();
        if !self.members.contains(&from) {
            self.members.push(from);
            out.extend(self.to_others(self.members_msg()));
        }
        if seq <= self.last_seq.get(&from).copied().unwrap_or(0) {
            // Already in the history; the author learns that by catching up.
            return out;
        }
        if base < self.hist_base || base > self.rev {
            out.push(Out::SendSnapshot(from));
            return out;
        }
        let mut c = op;
        for (_, _, h) in &self.history[(base - self.hist_base) as usize..] {
            match Op::transform(&c, h) {
                Some((c2, _)) => c = c2,
                None => {
                    out.push(Out::SendSnapshot(from));
                    return out;
                }
            }
        }
        self.push(from, seq, c.clone());
        out.push(Out::Apply(c.clone()));
        let m = Msg::Seq { sid: self.sid, gen: self.gen, rev: self.rev, author: from, seq, op: c.encode() };
        out.extend(self.to_others(m));
        out
    }

    /// Changes after `rev` as messages, or None if they are not kept.
    fn catch_up(&self, rev: u64) -> Option<Vec<Msg>> {
        if rev < self.hist_base || rev > self.rev {
            return None;
        }
        let from = (rev - self.hist_base) as usize;
        Some(
            self.history[from..]
                .iter()
                .enumerate()
                .map(|(k, (a, s, op))| Msg::Seq { sid: self.sid, gen: self.gen, rev: rev + 1 + k as u64, author: *a, seq: *s, op: op.encode() })
                .collect(),
        )
    }

    fn answer_resume(&mut self, from: u64, rev: u64) -> Vec<Out> {
        let mut out = Vec::new();
        if !self.members.contains(&from) {
            self.members.push(from);
            out.extend(self.to_others(self.members_msg()));
        } else {
            out.push(Out::Send(from, self.members_msg()));
        }
        match self.catch_up(rev) {
            Some(list) => {
                out.extend(list.into_iter().map(|m| Out::Send(from, m)));
                out.push(Out::Send(from, Msg::ResumeDone { sid: self.sid, gen: self.gen, rev: self.rev }));
            }
            None => out.push(Out::SendSnapshot(from)),
        }
        out
    }

    /// The host is gone: start choosing a new one.
    fn host_lost(&mut self, grace: bool) -> Vec<Out> {
        self.old_host = self.host;
        self.members.retain(|&m| m != self.old_host);
        self.cursors.remove(&self.old_host);
        self.votes.clear();
        self.votes.insert(self.me, Some(self.rev));
        self.voted = false;
        self.grace = grace;
        self.set_phase(Phase::Electing);
        if !grace {
            return self.cast_vote();
        }
        Vec::new()
    }

    fn cast_vote(&mut self) -> Vec<Out> {
        self.voted = true;
        let m = Msg::Elect { sid: self.sid, gen: self.gen, rev: self.rev };
        let mut out = self.to_others(m);
        out.extend(self.try_decide(false));
        out
    }

    fn try_decide(&mut self, timed_out: bool) -> Vec<Out> {
        if self.phase != Phase::Electing || !self.voted {
            return Vec::new();
        }
        let all = self.members.iter().all(|m| matches!(self.votes.get(m), Some(Some(_))));
        // Someone still reaches the old host: wait for them to notice too.
        let alive = self.members.iter().any(|m| matches!(self.votes.get(m), Some(None)));
        if !all && (!timed_out || alive) {
            return Vec::new();
        }
        // Most changes seen wins; the lower id breaks a tie.
        let winner = self
            .votes
            .iter()
            .filter_map(|(id, r)| r.map(|r| (*id, r)))
            .filter(|(id, _)| self.members.contains(id))
            .max_by_key(|(id, rev)| (*rev, std::cmp::Reverse(*id)))
            .map(|(id, _)| id)
            .unwrap_or(self.me);
        if winner == self.me {
            return self.become_host();
        }
        self.host = winner;
        self.set_phase(Phase::Resuming);
        vec![Out::Send(winner, Msg::Resume { sid: self.sid, gen: self.gen, rev: self.rev })]
    }

    /// Starts over from `host`'s text (after two hosts met).
    fn rejoin(&mut self, host: u64, why: &str) -> Vec<Out> {
        self.host = host;
        if !self.members.contains(&host) {
            self.members.push(host);
        }
        self.set_phase(Phase::Joining);
        vec![Out::Send(host, Msg::Join { sid: self.sid }), Out::Note(why.into())]
    }

    /// Two hosts of one session met through presence: compare notes.
    pub fn host_conflict(&mut self, other: u64) -> Vec<Out> {
        if self.is_host() && other != self.me {
            vec![Out::Send(other, self.members_msg())]
        } else {
            Vec::new()
        }
    }

    fn become_host(&mut self) -> Vec<Out> {
        self.host = self.me;
        self.set_phase(Phase::Live);
        // Our unconfirmed changes were never ordered (we saw the most), so
        // they are ordered now; the others get them when they catch up.
        if let Some((seq, op)) = self.awaiting.take() {
            self.push(self.me, seq, op);
        }
        if let Some(op) = self.pending.take() {
            let seq = self.next_seq;
            self.next_seq += 1;
            self.push(self.me, seq, op);
        }
        let mut out = self.to_others(self.members_msg());
        for (from, rev) in std::mem::take(&mut self.queued) {
            out.extend(self.answer_resume(from, rev));
        }
        out
    }

    pub fn on_msg(&mut self, from: u64, msg: Msg) -> Vec<Out> {
        match msg {
            Msg::Join { .. } => {
                if !self.is_host() {
                    return vec![Out::Send(from, Msg::Refuse { sid: self.sid, reason: "not the host".into() })];
                }
                if !self.members.contains(&from) {
                    self.members.push(from);
                }
                let mut out = vec![Out::SendSnapshot(from)];
                out.extend(self.to_others(self.members_msg()));
                out
            }
            Msg::Snapshot { gen, rev, host, members, text, .. } => {
                if self.phase == Phase::Live && self.host == self.me {
                    return Vec::new();
                }
                let lost = !self.in_step() && self.phase != Phase::Joining;
                self.gen = gen;
                self.rev = rev;
                self.hist_base = rev;
                self.history.clear();
                self.last_seq.clear();
                self.awaiting = None;
                self.pending = None;
                self.host = host;
                self.members = members;
                if !self.members.contains(&self.me) {
                    self.members.push(self.me);
                }
                self.set_phase(Phase::Live);
                let mut out = vec![Out::Install(text)];
                if lost {
                    out.push(Out::Note("Your last few changes could not be merged and were replaced by the shared text".into()));
                }
                out
            }
            Msg::Op { gen, seq, base, op, .. } => {
                if !self.is_host() {
                    // A client that has not heard of the new host yet.
                    return match self.phase {
                        Phase::Electing | Phase::Resuming => Vec::new(),
                        _ => vec![Out::Send(from, self.members_msg())],
                    };
                }
                if gen != self.gen {
                    return vec![Out::SendSnapshot(from)];
                }
                match Op::decode(&op) {
                    Some(op) => self.on_op(from, seq, base, op),
                    None => Vec::new(),
                }
            }
            Msg::Seq { gen, rev, author, seq, op, .. } => {
                if gen != self.gen || from != self.host || self.host == self.me || self.phase == Phase::Joining {
                    return Vec::new();
                }
                match Op::decode(&op) {
                    Some(op) => self.on_seq(rev, author, seq, op),
                    None => Vec::new(),
                }
            }
            Msg::Members { gen, rev, host, members, .. } => {
                if gen != self.gen {
                    return Vec::new();
                }
                if host == self.host {
                    if from == host {
                        self.members = members;
                        if !self.members.contains(&self.me) {
                            self.members.push(self.me);
                        }
                        let keep = self.members.clone();
                        self.cursors.retain(|k, _| keep.contains(k));
                    }
                    return Vec::new();
                }
                // Someone names a different host.
                if self.is_host() {
                    // Two hosts: the one that saw more (then the lower id) stays.
                    if from == host && (rev, std::cmp::Reverse(host)) > (self.rev, std::cmp::Reverse(self.me)) {
                        return self.rejoin(host, "Another copy of this session took over; reloading the shared text");
                    }
                    return if from == host { vec![Out::Send(from, self.members_msg())] } else { Vec::new() };
                }
                match self.phase {
                    // After an election every history is a prefix of the
                    // new host's, so catching up is enough.
                    Phase::Electing | Phase::Resuming if from == host => {
                        self.host = host;
                        self.members = members;
                        if !self.members.contains(&self.me) {
                            self.members.push(self.me);
                        }
                        self.set_phase(Phase::Resuming);
                        vec![Out::Send(host, Msg::Resume { sid: self.sid, gen: self.gen, rev: self.rev })]
                    }
                    // Our host handed over to another: start from its text.
                    Phase::Live if from == self.host => self.rejoin(host, "The session moved to another copy; reloading the shared text"),
                    _ => Vec::new(),
                }
            }
            Msg::Resume { gen, rev, .. } => {
                if gen != self.gen {
                    return if self.is_host() { vec![Out::SendSnapshot(from)] } else { Vec::new() };
                }
                if self.is_host() {
                    return self.answer_resume(from, rev);
                }
                if self.phase == Phase::Electing {
                    self.queued.push((from, rev));
                    return Vec::new();
                }
                vec![Out::Send(from, self.members_msg())]
            }
            Msg::ResumeDone { gen, rev, .. } => {
                if gen != self.gen || from != self.host || self.phase != Phase::Resuming || rev != self.rev {
                    return Vec::new();
                }
                self.set_phase(Phase::Live);
                // A change still unconfirmed was never ordered: send it again.
                if let Some((seq, op)) = &self.awaiting {
                    return vec![Out::Send(self.host, Msg::Op { sid: self.sid, gen: self.gen, seq: *seq, base: self.rev, op: op.encode() })];
                }
                self.try_send()
            }
            Msg::Elect { gen, rev, .. } => {
                if gen != self.gen {
                    return Vec::new();
                }
                if self.is_host() {
                    // They lost us for a moment; we are still here.
                    return vec![Out::Send(from, self.members_msg())];
                }
                match self.phase {
                    Phase::Electing => {
                        self.votes.insert(from, Some(rev));
                        if !self.members.contains(&from) {
                            self.members.push(from);
                        }
                        if self.voted {
                            self.try_decide(false)
                        } else {
                            Vec::new()
                        }
                    }
                    // We still reach the host: say so, and they wait.
                    Phase::Live if self.members.contains(&from) => vec![Out::Send(from, Msg::Alive { sid: self.sid, gen: self.gen, host: self.host })],
                    _ => Vec::new(),
                }
            }
            Msg::Alive { gen, .. } => {
                if gen == self.gen && self.phase == Phase::Electing && self.members.contains(&from) && !matches!(self.votes.get(&from), Some(Some(_))) {
                    self.votes.insert(from, None);
                }
                Vec::new()
            }
            Msg::Cursor { rev, anchor, head, .. } => {
                if self.members.contains(&from) {
                    self.cursors.insert(from, (rev, anchor, head));
                }
                Vec::new()
            }
            Msg::Saved { gen, rev, .. } => {
                let mut out = Vec::new();
                if gen != self.gen {
                    return out;
                }
                if self.is_host() {
                    let m = Msg::Saved { sid: self.sid, gen, rev };
                    out.extend(self.others().into_iter().filter(|&id| id != from).map(|id| Out::Send(id, m.clone())));
                }
                if rev == self.rev && self.in_step() {
                    out.push(Out::MarkSaved);
                }
                out
            }
            Msg::Leave { .. } => self.member_gone(from, false),
            Msg::Refuse { reason, .. } => {
                if self.phase == Phase::Joining {
                    return vec![Out::Stop(format!("Could not join: {reason}"))];
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// A member left the session or its PC dropped off. `dropped` allows a
    /// short wait in case the connection comes straight back.
    pub fn member_gone(&mut self, id: u64, dropped: bool) -> Vec<Out> {
        self.cursors.remove(&id);
        if id == self.me {
            return Vec::new();
        }
        if self.phase == Phase::Joining && id == self.host {
            return vec![Out::Stop("The person hosting the session went away".into())];
        }
        if id == self.host && self.host != self.me {
            if self.phase == Phase::Electing {
                return Vec::new();
            }
            return self.host_lost(dropped);
        }
        let was = self.members.len();
        self.members.retain(|&m| m != id);
        self.votes.remove(&id);
        let mut out = Vec::new();
        if self.is_host() && self.members.len() != was {
            out.extend(self.to_others(self.members_msg()));
        }
        out.extend(self.try_decide(false));
        out
    }

    /// A peer connected (again).
    pub fn peer_up(&mut self, id: u64) -> Vec<Out> {
        if self.phase == Phase::Electing && self.grace && !self.voted && id == self.old_host {
            // The host is back: catch up with it instead of electing.
            self.host = id;
            self.members.push(id);
            self.set_phase(Phase::Resuming);
            return vec![Out::Send(id, Msg::Resume { sid: self.sid, gen: self.gen, rev: self.rev })];
        }
        Vec::new()
    }

    /// Timeouts; call a few times a second while `busy()`.
    pub fn tick(&mut self) -> Vec<Out> {
        let t = self.since.elapsed();
        match self.phase {
            Phase::Electing if self.grace && !self.voted && t >= GRACE => self.cast_vote(),
            Phase::Electing if self.voted && t >= GRACE + ALIVE_WAIT => vec![Out::Stop("Lost contact with the person hosting the session".into())],
            Phase::Electing if self.voted && t >= GRACE + VOTE_WAIT => self.try_decide(true),
            Phase::Joining if t >= REPLY_WAIT => vec![Out::Stop("The session did not answer".into())],
            // The new host did not answer either: choose again without it.
            Phase::Resuming if t >= REPLY_WAIT => self.host_lost(false),
            _ => Vec::new(),
        }
    }

    pub fn busy(&self) -> bool {
        self.phase != Phase::Live
    }

    /// Leaving: tell the others.
    pub fn leave(&mut self) -> Vec<Out> {
        self.to_others(Msg::Leave { sid: self.sid })
    }

    /// This Slate's caret, when the text matches the agreed text.
    pub fn cursor(&mut self, anchor: u64, head: u64) -> Vec<Out> {
        if self.phase != Phase::Live || !self.in_step() {
            return Vec::new();
        }
        let c = (self.rev, anchor, head);
        if self.last_cursor == Some(c) {
            return Vec::new();
        }
        self.last_cursor = Some(c);
        self.to_others(Msg::Cursor { sid: self.sid, rev: c.0, anchor, head })
    }

    /// This Slate saved the file.
    pub fn saved(&mut self) -> Vec<Out> {
        if !self.in_step() || self.phase != Phase::Live {
            return Vec::new();
        }
        let m = Msg::Saved { sid: self.sid, gen: self.gen, rev: self.rev };
        if self.is_host() {
            self.to_others(m)
        } else {
            vec![Out::Send(self.host, m)]
        }
    }

    /// Where a position from revision `rev` is in the local text.
    pub fn map_remote(&self, rev: u64, p: usize) -> usize {
        let mut p = p;
        if rev >= self.hist_base {
            for (_, _, h) in self.history.iter().skip((rev - self.hist_base) as usize) {
                p = h.map_pos(p, false);
            }
        }
        if let Some((_, o)) = &self.awaiting {
            p = o.map_pos(p, false);
        }
        if let Some(o) = &self.pending {
            p = o.map_pos(p, false);
        }
        p
    }

    pub fn snapshot(&self, text: Vec<u8>) -> Msg {
        Msg::Snapshot { sid: self.sid, gen: self.gen, rev: self.rev, host: self.host, members: self.members.clone(), text }
    }
}

/// One buffer's part in a session.
pub struct Collab {
    pub core: Core,
    pub path: String,
    /// Local changes not yet turned into an operation: (length before,
    /// start, end, new text).
    pub outbox: Vec<(usize, usize, usize, Vec<u8>)>,
    /// Set while a remote change is applied, so it is not sent back.
    pub muted: bool,
}

impl Collab {
    pub fn joining(&self) -> bool {
        self.core.phase == Phase::Joining
    }
}

pub fn session_id(key: &str) -> u64 {
    key.bytes().fold(0xcbf29ce484222325u64, |h, c| (h ^ c as u64).wrapping_mul(0x100000001b3)) | 1
}

impl App {
    /// Sessions this Slate is in (published with presence).
    pub fn collab_sessions(&self) -> Vec<SessionInfo> {
        self.bufs
            .iter()
            .filter_map(|b| b.collab.as_ref())
            .filter(|c| !c.joining())
            .map(|c| SessionInfo { sid: c.core.sid, gen: c.core.gen, host: c.core.host, path: c.path.clone() })
            .collect()
    }

    fn collab_bi(&self, sid: u64) -> Option<usize> {
        self.bufs.iter().position(|b| b.collab.as_ref().map_or(false, |c| c.core.sid == sid))
    }

    /// The session others run on a file: (sid, gen, host, names in it).
    fn session_for(&self, bi: usize) -> Option<(u64, u64, u64, Vec<String>)> {
        let t = self.team.as_ref()?;
        let key = t.key_of(self.bufs[bi].path.as_ref()?);
        let sid = session_id(&key);
        // Several lineages can exist for a moment; the lowest gen wins.
        let mut best: Option<(u64, u64)> = None;
        let mut names = Vec::new();
        for p in t.people() {
            for s in p.sessions.iter().filter(|s| s.sid == sid) {
                if best.map_or(true, |(g, _)| s.gen < g) {
                    best = Some((s.gen, s.host));
                }
                if !names.contains(&p.name) {
                    names.push(p.name.clone());
                }
            }
        }
        best.map(|(g, h)| (sid, g, h, names))
    }

    /// "Ravi is" / "Ravi and Anil are" when others co-edit this file.
    pub fn collab_offer_for(&self, bi: usize) -> Option<String> {
        if self.bufs[bi].collab.is_some() {
            return None;
        }
        let (_, _, _, names) = self.session_for(bi)?;
        let n: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        Some(format!("{} {}", team::join_names(&n), if n.len() == 1 { "is" } else { "are" }))
    }

    /// Ctrl+Shift+E: start, join or leave co-editing the current file.
    pub fn collab_toggle(&mut self) {
        let bi = self.bi(self.tab(self.av).buf);
        if self.bufs[bi].collab.is_some() {
            self.collab_leave(bi);
            self.team_publish();
            return self.flash("You left co-editing; the file stays open with the shared text");
        }
        let Some(t) = &self.team else {
            return self.flash("Team features are off \u{2013} Settings: Toggle Team Features");
        };
        let b = &self.bufs[bi];
        if b.path.is_none() || b.diff.is_some() || b.loading.is_some() {
            return self.flash("Co-editing works on saved files");
        }
        if let Some(by) = b.locked_by.clone() {
            return self.flash(&format!("{by} has locked this file"));
        }
        if b.doc.len() > crate::wire::MAX_FRAME / 2 {
            return self.flash("This file is too large to co-edit");
        }
        let me = t.me;
        let path = t.shared(b.path.as_ref().unwrap());
        match self.session_for(bi) {
            Some((sid, _gen, host, names)) if t.net.connected(host) => {
                if self.bufs[bi].dirty() && !crate::sys::ask_yes_no(self.hwnd, "Join co-editing?\n\nYour unsaved changes to this file are replaced by the shared text.") {
                    return;
                }
                let (core, outs) = Core::join(me, sid, host);
                self.bufs[bi].collab = Some(Box::new(Collab { core, path, outbox: Vec::new(), muted: false }));
                self.bufs[bi].lock_since = None;
                self.collab_run(bi, outs);
                let n: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                self.flash(&format!("Joining {}\u{2026}", team::join_names(&n)));
            }
            Some(_) => self.flash("The person hosting this session is not reachable right now"),
            None => {
                let sid = session_id(&t.key_of(b.path.as_ref().unwrap()));
                let gen = crate::net::new_id();
                self.bufs[bi].collab = Some(Box::new(Collab { core: Core::host(me, sid, gen), path, outbox: Vec::new(), muted: false }));
                self.bufs[bi].lock_since = None;
                self.flash("Co-editing on: others who open this file can join with Ctrl+Shift+E");
            }
        }
        self.team_publish();
        self.collab_timer();
        self.after_switch();
    }

    /// Leaves the session of buffer `bi` (also when its tab closes).
    pub fn collab_leave(&mut self, bi: usize) {
        if let Some(mut c) = self.bufs[bi].collab.take() {
            let outs = c.core.leave();
            if let Some(t) = &self.team {
                for o in outs {
                    if let Out::Send(to, m) = o {
                        t.net.send(to, &m);
                    }
                }
            }
        }
    }

    /// Carries out what a session asked for.
    fn collab_run(&mut self, bi: usize, outs: Vec<Out>) {
        for o in outs {
            match o {
                Out::Send(to, m) => {
                    if let Some(t) = &self.team {
                        t.net.send(to, &m);
                    }
                }
                Out::SendSnapshot(to) => {
                    let text = self.bufs[bi].text_all();
                    if let (Some(c), Some(t)) = (self.bufs[bi].collab.as_ref(), self.team.as_ref()) {
                        t.net.send(to, &c.core.snapshot(text));
                    }
                }
                Out::Apply(op) => {
                    let b = &mut self.bufs[bi];
                    if op.base == b.doc.len() {
                        b.apply_remote(&op);
                    } else if let Some(c) = b.collab.as_mut() {
                        // Out of step (should not happen): reload the shared text.
                        let host = c.core.host;
                        c.core.phase = Phase::Joining;
                        if let Some(t) = &self.team {
                            t.net.send(host, &Msg::Join { sid: c.core.sid });
                        }
                    }
                }
                Out::Install(text) => self.collab_install(bi, text),
                Out::MarkSaved => {
                    let b = &mut self.bufs[bi];
                    b.mark_saved();
                    b.mtime = b.path.as_ref().and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
                }
                Out::Note(s) => self.flash(&s),
                Out::Stop(s) => {
                    self.bufs[bi].collab = None;
                    self.flash(&format!("{s} \u{2013} co-editing stopped; your text is kept"));
                    self.team_publish();
                }
            }
        }
    }

    /// Puts the shared text into the buffer.
    fn collab_install(&mut self, bi: usize, text: Vec<u8>) {
        let on_disk = self.bufs[bi].path.as_ref().and_then(|p| crate::doc::read_for_doc(p).ok()).map(|raw| crate::enc::decode(raw, None).text);
        let same = on_disk.as_deref() == Some(&text[..]);
        let b = &mut self.bufs[bi];
        let line = b.doc.line_of(b.caret());
        if let Some(c) = b.collab.as_mut() {
            c.muted = true;
            c.outbox.clear();
        }
        b.replace_all_quiet(text);
        if let Some(c) = b.collab.as_mut() {
            c.muted = false;
        }
        let l = line.min(b.doc.line_count() - 1);
        let p = b.doc.line_start(l);
        b.set_caret(p);
        if same {
            b.mark_saved();
        } else {
            b.mark_unsaved();
        }
        let names = b.collab.as_ref().map(|c| c.core.others()).unwrap_or_default();
        let who: Vec<String> = names.iter().filter_map(|id| self.team.as_ref()?.peers.get(id).map(|p| p.name.clone())).collect();
        let w: Vec<&str> = who.iter().map(|s| s.as_str()).collect();
        if !w.is_empty() {
            self.flash(&format!("Co-editing with {}", team::join_names(&w)));
        }
        self.team_publish();
        self.after_switch();
    }

    pub fn collab_on_msg(&mut self, from: u64, m: Msg) {
        let sid = match &m {
            Msg::Join { sid } | Msg::Snapshot { sid, .. } | Msg::Op { sid, .. } | Msg::Seq { sid, .. } | Msg::Members { sid, .. } | Msg::Cursor { sid, .. } | Msg::Saved { sid, .. } | Msg::Leave { sid } | Msg::Elect { sid, .. } | Msg::Resume { sid, .. } | Msg::ResumeDone { sid, .. } | Msg::Refuse { sid, .. } => *sid,
            _ => return,
        };
        // Local edits go first, so the order stays right.
        self.collab_flush();
        let Some(bi) = self.collab_bi(sid) else {
            if let Msg::Join { sid } = m {
                if let Some(t) = &self.team {
                    t.net.send(from, &Msg::Refuse { sid, reason: "the session has ended".into() });
                }
            }
            return;
        };
        let outs = self.bufs[bi].collab.as_mut().unwrap().core.on_msg(from, m);
        let applied = outs.iter().any(|o| matches!(o, Out::Apply(_)));
        self.collab_run(bi, outs);
        if applied {
            self.edited();
            self.recount_if_open();
        }
        self.collab_timer();
        self.team_publish();
        self.update_title();
    }

    pub fn collab_peer_down(&mut self, id: u64) {
        for bi in 0..self.bufs.len() {
            let Some(c) = self.bufs[bi].collab.as_mut() else { continue };
            if !c.core.members.contains(&id) && c.core.host != id {
                continue;
            }
            let outs = c.core.member_gone(id, true);
            self.collab_run(bi, outs);
        }
        self.collab_timer();
        self.team_publish();
    }

    pub fn collab_peer_up(&mut self, id: u64) {
        for bi in 0..self.bufs.len() {
            let Some(c) = self.bufs[bi].collab.as_mut() else { continue };
            let outs = c.core.peer_up(id);
            self.collab_run(bi, outs);
        }
    }

    /// Runs session timeouts (elections, joining) while any is under way.
    pub fn collab_tick(&mut self) {
        for bi in 0..self.bufs.len() {
            let Some(c) = self.bufs[bi].collab.as_mut() else { continue };
            let was = c.core.phase;
            let outs = c.core.tick();
            self.collab_run(bi, outs);
            let now = self.bufs[bi].collab.as_ref().map(|c| c.core.phase);
            if was != Phase::Live && now == Some(Phase::Live) {
                let host_is_me = self.bufs[bi].collab.as_ref().map_or(false, |c| c.core.host == c.core.me);
                self.flash(if host_is_me { "You now keep the co-editing session going" } else { "Co-editing reconnected" });
            }
        }
        self.collab_timer();
        self.team_publish();
        self.redraw();
    }

    fn collab_timer(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};
        let busy = self.bufs.iter().any(|b| b.collab.as_ref().map_or(false, |c| c.core.busy()));
        unsafe {
            if busy {
                SetTimer(self.hwnd, T_COLLAB, 250, None);
            } else {
                let _ = KillTimer(self.hwnd, T_COLLAB);
            }
        }
    }

    /// Sends local edits made since the last call.
    pub fn collab_flush(&mut self) {
        for bi in 0..self.bufs.len() {
            let Some(c) = self.bufs[bi].collab.as_mut() else { continue };
            if c.outbox.is_empty() {
                continue;
            }
            let mut op: Option<Op> = None;
            for (len, a, z, t) in c.outbox.drain(..) {
                let o = Op::replace(len, a, z, &t);
                op = Some(match op {
                    Some(p) => p.compose(&o).unwrap_or(o),
                    None => o,
                });
            }
            if let Some(op) = op {
                let outs = c.core.local(op);
                self.collab_run(bi, outs);
            }
        }
        self.collab_cursor();
    }

    /// Publishes the caret of the current co-edited buffer when it moved.
    pub fn collab_cursor(&mut self) {
        if self.views[self.av].tabs.is_empty() {
            return;
        }
        let bi = self.bi(self.tab(self.av).buf);
        let s = self.bufs[bi].primary();
        let Some(c) = self.bufs[bi].collab.as_mut() else { return };
        let outs = c.core.cursor(s.anchor as u64, s.head as u64);
        self.collab_run(bi, outs);
    }

    pub fn collab_saved(&mut self, id: u32) {
        let bi = self.bi(id);
        let Some(c) = self.bufs[bi].collab.as_mut() else { return };
        let outs = c.core.saved();
        self.collab_run(bi, outs);
    }

    /// Reacts to someone's new presence: offers to join a session on an
    /// open file once, and merges two sessions started at the same moment.
    pub fn collab_on_presence(&mut self, from: u64) {
        let mut offers = Vec::new();
        for bi in 0..self.bufs.len() {
            if self.bufs[bi].path.is_none() {
                continue;
            }
            let Some((sid, gen, host, names)) = self.session_for(bi) else {
                continue;
            };
            match self.bufs[bi].collab.as_ref() {
                None => {
                    if self.collab_offered.get(&sid) != Some(&gen) {
                        self.collab_offered.insert(sid, gen);
                        let n: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                        offers.push(format!("{} {} co-editing {} \u{2013} Ctrl+Shift+E joins", team::join_names(&n), if n.len() == 1 { "is" } else { "are" }, self.bufs[bi].name()));
                    }
                }
                Some(c) if c.core.sid == sid && gen < c.core.gen && !c.joining() => {
                    // Two sessions started at once: the lower lineage wins.
                    let me = c.core.me;
                    self.collab_leave(bi);
                    let path = self.team.as_ref().map(|t| t.shared(self.bufs[bi].path.as_ref().unwrap())).unwrap_or_default();
                    let (core, outs) = Core::join(me, sid, host);
                    self.bufs[bi].collab = Some(Box::new(Collab { core, path, outbox: Vec::new(), muted: false }));
                    self.collab_run(bi, outs);
                    self.flash("Someone started co-editing this file at the same moment \u{2013} joining their session");
                }
                _ => {}
            }
        }
        if let Some(o) = offers.pop() {
            self.flash(&o);
        }
        // `from` hosts a session we also host (one of us was cut off and
        // carried on alone): the two hosts compare notes and one gives way.
        let theirs: Vec<SessionInfo> = self.team.as_ref().and_then(|t| t.peers.get(&from)).map(|p| p.sessions.clone()).unwrap_or_default();
        for s in theirs.into_iter().filter(|s| s.host == from) {
            if let Some(bi) = self.collab_bi(s.sid) {
                let c = self.bufs[bi].collab.as_mut().unwrap();
                if c.core.gen == s.gen && c.core.host != from {
                    let outs = c.core.host_conflict(from);
                    self.collab_run(bi, outs);
                }
            }
        }
        self.collab_timer();
    }

    /// Others' carets and selections in buffer `bi`:
    /// (anchor, head, name, colour), in local positions.
    pub fn collab_carets(&self, bi: usize) -> Vec<(usize, usize, String, u32)> {
        let (Some(c), Some(t)) = (self.bufs[bi].collab.as_ref(), self.team.as_ref()) else { return Vec::new() };
        let len = self.bufs[bi].doc.len();
        c.core
            .cursors
            .iter()
            .filter_map(|(id, &(rev, a, h))| {
                let p = t.peers.get(id)?;
                Some((c.core.map_remote(rev, a as usize).min(len), c.core.map_remote(rev, h as usize).min(len), p.name.clone(), p.color()))
            })
            .collect()
    }

    /// Names of the others in the session of buffer `bi`.
    pub fn collab_people(&self, bi: usize) -> Vec<String> {
        let (Some(c), Some(t)) = (self.bufs[bi].collab.as_ref(), self.team.as_ref()) else { return Vec::new() };
        let mut v: Vec<String> = c.core.others().iter().filter_map(|id| t.peers.get(id).map(|p| p.name.clone())).collect();
        v.dedup();
        v
    }

    /// Status-bar text of a co-edited buffer.
    pub fn collab_status(&self, bi: usize) -> Option<String> {
        let c = self.bufs[bi].collab.as_ref()?;
        let people = self.collab_people(bi);
        let who: Vec<&str> = people.iter().map(|s| s.as_str()).collect();
        Some(match c.core.phase {
            Phase::Joining => "Co-editing \u{2013} joining\u{2026}".to_string(),
            Phase::Electing | Phase::Resuming => "Co-editing \u{2013} reconnecting\u{2026}".to_string(),
            Phase::Live if who.is_empty() => "Co-editing \u{2013} waiting for others".to_string(),
            Phase::Live => format!("Co-editing with {}", team::join_names(&who)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A simulated network: per-pair FIFO queues, delivered in random
    /// order between pairs; a participant can crash at any point.
    struct Sim {
        cores: Vec<Option<Core>>,
        texts: Vec<Vec<u8>>,
        queues: HashMap<(u64, u64), VecDeque<Msg>>,
        seed: u64,
    }

    impl Sim {
        fn rnd(&mut self) -> usize {
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 7;
            self.seed ^= self.seed << 17;
            (self.seed >> 11) as usize
        }
        fn id(k: usize) -> u64 {
            100 + k as u64
        }
        fn idx(id: u64) -> usize {
            (id - 100) as usize
        }
        fn run(&mut self, k: usize, outs: Vec<Out>) {
            for o in outs {
                match o {
                    Out::Send(to, m) => self.queues.entry((Self::id(k), to)).or_default().push_back(m),
                    Out::SendSnapshot(to) => {
                        let m = self.cores[k].as_ref().unwrap().snapshot(self.texts[k].clone());
                        self.queues.entry((Self::id(k), to)).or_default().push_back(m);
                    }
                    Out::Apply(op) => self.texts[k] = op.apply(&self.texts[k]).expect("op fits"),
                    Out::Install(t) => self.texts[k] = t,
                    Out::Stop(s) => panic!("stopped: {s}"),
                    Out::MarkSaved | Out::Note(_) => {}
                }
            }
        }
        fn alive(&self) -> Vec<usize> {
            (0..self.cores.len()).filter(|&k| self.cores[k].is_some()).collect()
        }
        fn edit(&mut self, k: usize, tag: u8) {
            if self.cores[k].as_ref().unwrap().phase == Phase::Joining {
                return;
            }
            let len = self.texts[k].len();
            let a = self.rnd() % (len + 1);
            let z = (a + self.rnd() % 3).min(len);
            let op = Op::replace(len, a, z, &[tag]);
            self.texts[k] = op.apply(&self.texts[k]).unwrap();
            let outs = self.cores[k].as_mut().unwrap().local(op);
            self.run(k, outs);
        }
        /// Delivers one queued message, if any.
        fn deliver(&mut self) -> bool {
            let keys: Vec<(u64, u64)> = self.queues.iter().filter(|(_, q)| !q.is_empty()).map(|(k, _)| *k).collect();
            if keys.is_empty() {
                return false;
            }
            let (from, to) = keys[self.rnd() % keys.len()];
            let m = self.queues.get_mut(&(from, to)).unwrap().pop_front().unwrap();
            let k = Self::idx(to);
            if self.cores[k].is_some() {
                let outs = self.cores[k].as_mut().unwrap().on_msg(from, m);
                self.run(k, outs);
            }
            true
        }
        fn crash(&mut self, k: usize) {
            self.cores[k] = None;
            let dead = Self::id(k);
            self.queues.retain(|(f, t), _| *f != dead && *t != dead);
            for j in self.alive() {
                let outs = self.cores[j].as_mut().unwrap().member_gone(dead, false);
                self.run(j, outs);
            }
        }
        /// Lets every timeout fire (elections finish).
        fn expire(&mut self) {
            for j in self.alive() {
                let c = self.cores[j].as_mut().unwrap();
                c.since -= GRACE + VOTE_WAIT + Duration::from_millis(10);
                let outs = c.tick();
                self.run(j, outs);
            }
        }
        fn settle(&mut self) {
            for _ in 0..50 {
                while self.deliver() {}
                self.expire();
                while self.deliver() {}
                if self.alive().iter().all(|&k| self.cores[k].as_ref().unwrap().phase == Phase::Live && self.cores[k].as_ref().unwrap().in_step()) {
                    return;
                }
            }
        }
    }

    fn session(n: usize, seed: u64) -> Sim {
        let base = b"PIPE\nELBOW\nFLANGE\n".to_vec();
        let mut s = Sim { cores: Vec::new(), texts: vec![base; n], queues: HashMap::new(), seed };
        s.cores.push(Some(Core::host(Sim::id(0), 9, 77)));
        for k in 1..n {
            let (c, outs) = Core::join(Sim::id(k), 9, Sim::id(0));
            s.cores.push(Some(c));
            s.run(k, outs);
        }
        s.settle();
        s
    }

    fn assert_converged(s: &Sim) {
        let alive = s.alive();
        for &k in &alive {
            let c = s.cores[k].as_ref().unwrap();
            assert_eq!(c.phase, Phase::Live, "member {k} not live");
            assert!(c.in_step(), "member {k} has unconfirmed changes");
            assert_eq!(s.texts[k], s.texts[alive[0]], "member {k} differs");
        }
        let hosts: Vec<u64> = alive.iter().map(|&k| s.cores[k].as_ref().unwrap().host).collect();
        assert!(hosts.iter().all(|&h| h == hosts[0]), "one host for all: {hosts:?}");
    }

    #[test]
    fn concurrent_editing_converges() {
        for seed in 1..40u64 {
            let mut s = session(4, seed * 7919);
            for step in 0..300 {
                let k = s.rnd() % 4;
                if s.rnd() % 2 == 0 {
                    s.edit(k, b'a' + (step % 26) as u8);
                } else {
                    s.deliver();
                }
            }
            s.settle();
            assert_converged(&s);
        }
    }

    #[test]
    fn host_crash_mid_edit_converges_without_losing_client_edits() {
        for seed in 1..60u64 {
            let mut s = session(4, seed * 104729);
            for step in 0..120 {
                let k = 1 + s.rnd() % 3;
                if s.rnd() % 2 == 0 {
                    s.edit(k, b'A' + (step % 26) as u8);
                } else {
                    s.deliver();
                }
            }
            // Clients' typed letters must all survive the host's crash.
            s.crash(0);
            for step in 0..120 {
                let alive = s.alive();
                let k = alive[s.rnd() % alive.len()];
                if s.rnd() % 2 == 0 {
                    s.edit(k, b'a' + (step % 26) as u8);
                } else {
                    s.deliver();
                }
                if step == 60 {
                    s.expire();
                }
            }
            s.settle();
            assert_converged(&s);
            // And the new host can be lost too.
            let host = Sim::idx(s.cores[s.alive()[0]].as_ref().unwrap().host);
            s.crash(host);
            for step in 0..60 {
                let alive = s.alive();
                let k = alive[s.rnd() % alive.len()];
                if s.rnd() % 3 == 0 {
                    s.edit(k, b'0' + (step % 10) as u8);
                } else {
                    s.deliver();
                }
            }
            s.settle();
            assert_converged(&s);
        }
    }

    #[test]
    fn duplicate_resend_is_applied_once() {
        let mut s = session(2, 5);
        s.edit(1, b'X');
        // Host orders it, but the confirmation never reaches the client.
        while s.deliver() {
            let q = s.queues.get_mut(&(Sim::id(0), Sim::id(1)));
            if let Some(q) = q {
                q.clear();
            }
        }
        // The client resends after a catch-up: still one X.
        let rev = s.cores[1].as_ref().unwrap().rev;
        let outs = s.cores[0].as_mut().unwrap().on_msg(Sim::id(1), Msg::Resume { sid: 9, gen: 77, rev });
        s.run(0, outs);
        s.cores[1].as_mut().unwrap().phase = Phase::Resuming;
        s.settle();
        assert_converged(&s);
        assert_eq!(s.texts[0].iter().filter(|&&c| c == b'X').count(), 1);
    }

    #[test]
    fn staggered_host_loss_waits_for_everyone() {
        for seed in 1..30u64 {
            let mut s = session(3, seed * 31337);
            for step in 0..60 {
                let k = 1 + s.rnd() % 2;
                if s.rnd() % 2 == 0 {
                    s.edit(k, b'a' + (step % 26) as u8);
                } else {
                    s.deliver();
                }
            }
            // The host's PC dies; member 1 notices first.
            s.cores[0] = None;
            let dead = Sim::id(0);
            s.queues.retain(|(f, t), _| *f != dead && *t != dead);
            let outs = s.cores[1].as_mut().unwrap().member_gone(dead, false);
            s.run(1, outs);
            while s.deliver() {}
            // Member 2 still believes in the host, so 1 must not decide.
            {
                let c = s.cores[1].as_mut().unwrap();
                c.since -= GRACE + VOTE_WAIT + Duration::from_millis(10);
                let outs = c.tick();
                s.run(1, outs);
            }
            assert_eq!(s.cores[1].as_ref().unwrap().phase, Phase::Electing);
            s.edit(2, b'Z');
            // Now member 2 notices too.
            let outs = s.cores[2].as_mut().unwrap().member_gone(dead, false);
            s.run(2, outs);
            s.settle();
            assert_converged(&s);
            assert!(s.texts[1].contains(&b'Z'));
        }
    }

    #[test]
    fn cut_off_member_that_carried_on_alone_gives_way() {
        let mut s = session(3, 99);
        for _ in 0..10 {
            s.edit(1, b'x');
            s.deliver();
        }
        s.settle();
        // Member 2 loses the network: it sees everyone go and hosts alone.
        for id in [Sim::id(0), Sim::id(1)] {
            let outs = s.cores[2].as_mut().unwrap().member_gone(id, false);
            s.run(2, outs);
        }
        s.queues.retain(|(f, t), _| *f != Sim::id(2) && *t != Sim::id(2));
        {
            let c = s.cores[2].as_mut().unwrap();
            c.since -= GRACE + VOTE_WAIT + Duration::from_millis(10);
            let outs = c.tick();
            s.run(2, outs);
        }
        assert!(s.cores[2].as_ref().unwrap().is_host());
        s.edit(2, b'L');
        // The others carry on, and remove member 2 as gone.
        for k in [0, 1] {
            let outs = s.cores[k].as_mut().unwrap().member_gone(Sim::id(2), false);
            s.run(k, outs);
        }
        for _ in 0..10 {
            s.edit(1, b'y');
            s.deliver();
        }
        s.settle();
        // Back on the network: presence shows two hosts of one session.
        let outs = s.cores[2].as_mut().unwrap().host_conflict(Sim::id(0));
        s.run(2, outs);
        s.settle();
        assert_converged(&s);
        assert_eq!(s.cores[2].as_ref().unwrap().host, Sim::id(0));
    }

    #[test]
    fn late_joiner_gets_the_text_and_follows() {
        let mut s = session(2, 11);
        for _ in 0..20 {
            s.edit(0, b'h');
            s.edit(1, b'c');
            s.deliver();
        }
        s.settle();
        s.texts.push(b"stale".to_vec());
        let (c, outs) = Core::join(Sim::id(2), 9, Sim::id(0));
        s.cores.push(Some(c));
        s.run(2, outs);
        s.settle();
        s.edit(2, b'J');
        s.settle();
        assert_converged(&s);
        assert!(s.texts[0].contains(&b'J'));
    }
}
