//! Direct LAN connections between Slate instances, with no setup.
//!
//! Discovery: every Slate listens on UDP port 47613 (shared, so several
//! Slates on one PC all hear it) for multicast and broadcast beacons, and
//! announces itself from a second, private UDP socket. Anyone who hears a
//! beacon that asks for answers replies straight to that private socket, so
//! a PC whose firewall blocks unsolicited traffic still learns about the
//! others (Windows lets unicast replies to its own broadcasts through).
//!
//! Connections: each pair of Slates keeps one TCP connection, made by
//! whichever side reaches the other first; if both connect at once, the
//! one opened by the lower id survives on both sides. The kernel's TCP
//! keep-alive notices PCs that vanish, so nothing polls while idle.

use crate::wire::{Beacon, Msg, MAX_FRAME, PROTOCOL};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the window with a `Box<Event>` in lParam.
pub const WM_NET: u32 = WM_APP + 6;
pub const UDP_PORT: u16 = 47613;
/// Administratively scoped multicast group (stays inside the site).
pub const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 71, 13);
/// Beacons repeat this often so a missed packet heals itself.
const BEACON_EVERY: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum Event {
    /// A peer finished the handshake.
    Up { id: u64, name: String, pc: String, version: String, hub: bool },
    Down { id: u64 },
    Msg { from: u64, msg: Msg },
}

/// Where events go: the editor window, or a channel (the search hub).
#[derive(Clone)]
pub enum Sink {
    Window(isize),
    Channel(mpsc::Sender<Event>),
}

impl Sink {
    fn post(&self, e: Event) {
        match self {
            Sink::Window(h) => {
                let p = Box::into_raw(Box::new(e));
                unsafe {
                    if PostMessageW(HWND(*h as *mut _), WM_NET, WPARAM(0), LPARAM(p as isize)).is_err() {
                        drop(Box::from_raw(p));
                    }
                }
            }
            Sink::Channel(tx) => {
                let _ = tx.send(e);
            }
        }
    }
}

struct Conn {
    serial: u64,
    initiator: u64,
    tx: mpsc::Sender<Arc<Vec<u8>>>,
    sock: TcpStream,
}

#[derive(Default)]
struct Registry {
    conns: HashMap<u64, Conn>,
    /// Where each peer said it listens (from beacons).
    addrs: HashMap<u64, SocketAddr>,
    connecting: HashSet<u64>,
}

struct Shared {
    me: u64,
    hello: Msg,
    sink: Sink,
    reg: Mutex<Registry>,
    stop: AtomicBool,
    serial: AtomicU64,
    port: u16,
}

pub struct Net {
    sh: Arc<Shared>,
    beacon_tx: UdpSocket,
}

/// A fresh random id for this run of Slate.
pub fn new_id() -> u64 {
    let mut b = [0u8; 8];
    unsafe {
        use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
        let _ = BCryptGenRandom(None, &mut b, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
    }
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    // Never 0: 0 means "nobody" in messages.
    (u64::from_le_bytes(b) ^ t.rotate_left(17) ^ std::process::id() as u64) | 1
}

/// IPv4 addresses of the PC's working network adapters, with their
/// broadcast addresses.
pub fn interfaces() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    use windows::Win32::NetworkManagement::IpHelper::{GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH};
    use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
    let mut out = Vec::new();
    let mut len = 16 * 1024u32;
    let mut buf = vec![0u64; len as usize / 8];
    unsafe {
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut r = GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut len);
        if r == 111 {
            // ERROR_BUFFER_OVERFLOW: retry with the size asked for.
            buf = vec![0u64; len as usize / 8 + 1];
            r = GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut len);
        }
        if r != 0 {
            return out;
        }
        let mut a = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !a.is_null() {
            let ad = &*a;
            // Skip adapters that are down and the loopback adapter (type 24).
            if ad.OperStatus == IfOperStatusUp && ad.IfType != 24 {
                let mut u = ad.FirstUnicastAddress;
                while !u.is_null() {
                    let ua = &*u;
                    let sa = ua.Address.lpSockaddr;
                    if !sa.is_null() && (*sa).sa_family == AF_INET {
                        let sin = &*(sa as *const SOCKADDR_IN);
                        let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.S_un.S_addr));
                        let bits = ua.OnLinkPrefixLength.min(32) as u32;
                        let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
                        let bc = Ipv4Addr::from(u32::from(ip) | !mask);
                        if !ip.is_loopback() && !ip.is_link_local() {
                            out.push((ip, bc));
                        }
                    }
                    u = ua.Next;
                }
            }
            a = ad.Next;
        }
    }
    out
}

/// A UDP socket on the shared discovery port. SO_REUSEADDR has to be set
/// before binding, which std cannot do, hence the raw socket calls.
fn shared_udp() -> std::io::Result<UdpSocket> {
    use std::os::windows::io::FromRawSocket;
    use windows::Win32::Networking::WinSock::{bind, setsockopt, WSASocketW, AF_INET, IN_ADDR, IPPROTO_UDP, SOCKADDR, SOCKADDR_IN, SOCK_DGRAM, SOL_SOCKET, SO_REUSEADDR, WSA_FLAG_OVERLAPPED};
    // Makes sure WinSock is started (std does it on first use).
    drop(UdpSocket::bind("127.0.0.1:0"));
    unsafe {
        let s = WSASocketW(AF_INET.0 as i32, SOCK_DGRAM.0, IPPROTO_UDP.0, None, 0, WSA_FLAG_OVERLAPPED).map_err(|_| std::io::Error::last_os_error())?;
        let one = 1i32.to_ne_bytes();
        setsockopt(s, SOL_SOCKET, SO_REUSEADDR, Some(&one));
        let mut sin = SOCKADDR_IN::default();
        sin.sin_family = AF_INET;
        sin.sin_port = UDP_PORT.to_be();
        sin.sin_addr = IN_ADDR::default();
        if bind(s, &sin as *const SOCKADDR_IN as *const SOCKADDR, std::mem::size_of::<SOCKADDR_IN>() as i32) != 0 {
            let e = std::io::Error::last_os_error();
            let _ = windows::Win32::Networking::WinSock::closesocket(s);
            return Err(e);
        }
        Ok(UdpSocket::from_raw_socket(s.0 as u64))
    }
}

/// Short keep-alive, so a PC that loses power or the network is noticed
/// in about half a minute instead of two hours.
fn keepalive(s: &TcpStream) {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Networking::WinSock::{tcp_keepalive, WSAIoctl, SIO_KEEPALIVE_VALS, SOCKET};
    let ka = tcp_keepalive { onoff: 1, keepalivetime: 15_000, keepaliveinterval: 3_000 };
    let mut ret = 0u32;
    unsafe {
        WSAIoctl(
            SOCKET(s.as_raw_socket() as usize),
            SIO_KEEPALIVE_VALS,
            Some(&ka as *const _ as *const _),
            std::mem::size_of::<tcp_keepalive>() as u32,
            None,
            0,
            &mut ret,
            None,
            None,
        );
    }
    let _ = s.set_nodelay(true);
}

fn read_frame(s: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut b = vec![0u8; n];
    s.read_exact(&mut b)?;
    Ok(b)
}

impl Net {
    /// Starts listening and announcing. `hub` marks a search hub.
    pub fn start(me: u64, name: String, pc: String, hub: bool, sink: Sink) -> std::io::Result<Net> {
        let listener = TcpListener::bind("0.0.0.0:0")?;
        let port = listener.local_addr()?.port();
        let hello = Msg::Hello { id: me, protocol: PROTOCOL, name, pc, version: env!("CARGO_PKG_VERSION").into(), hub };
        let sh = Arc::new(Shared { me, hello, sink, reg: Mutex::new(Registry::default()), stop: AtomicBool::new(false), serial: AtomicU64::new(1), port });
        // Beacons go out from a private socket, so direct replies reach this
        // Slate and not another one on the same PC.
        let beacon_tx = UdpSocket::bind("0.0.0.0:0")?;
        beacon_tx.set_broadcast(true)?;
        let _ = beacon_tx.set_multicast_ttl_v4(1);
        {
            let sh = sh.clone();
            spawn("slate-accept", move || {
                for s in listener.incoming() {
                    if sh.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Ok(s) = s {
                        let sh = sh.clone();
                        spawn("slate-peer", move || handshake(sh, s, false));
                    }
                }
            });
        }
        // Listener on the shared port: hears everyone's beacons.
        if let Ok(rx) = shared_udp() {
            for (ip, _) in interfaces() {
                let _ = rx.join_multicast_v4(&GROUP, &ip);
            }
            let _ = rx.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED);
            let reply = beacon_tx.try_clone()?;
            let sh = sh.clone();
            spawn("slate-discover", move || listen_beacons(sh, rx, reply));
        }
        // Replies to our own queries arrive on the private socket.
        {
            let sock = beacon_tx.try_clone()?;
            let sh = sh.clone();
            spawn("slate-replies", move || {
                let mut buf = [0u8; 64];
                while !sh.stop.load(Ordering::Relaxed) {
                    if let Ok((n, from)) = sock.recv_from(&mut buf) {
                        if let Some(b) = Beacon::decode(&buf[..n]) {
                            heard(&sh, b, from);
                        }
                    }
                }
            });
        }
        let net = Net { sh, beacon_tx };
        net.announce(true);
        // Repeat the beacon now and then, in case a datagram was lost.
        {
            let sock = net.beacon_tx.try_clone()?;
            let sh = net.sh.clone();
            spawn("slate-beacon", move || {
                let mut last = Instant::now();
                while !sh.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    if last.elapsed() >= BEACON_EVERY {
                        last = Instant::now();
                        send_beacon(&sock, &sh, false);
                    }
                }
            });
        }
        Ok(net)
    }

    #[cfg(test)]
    pub fn me(&self) -> u64 {
        self.sh.me
    }

    /// Sends a beacon; with `query`, everyone answers straight back.
    pub fn announce(&self, query: bool) {
        send_beacon(&self.beacon_tx, &self.sh, query);
    }

    pub fn send(&self, to: u64, m: &Msg) -> bool {
        let f = Arc::new(m.frame());
        let reg = self.sh.reg.lock().unwrap();
        match reg.conns.get(&to) {
            Some(c) => c.tx.send(f).is_ok(),
            None => false,
        }
    }

    pub fn broadcast(&self, m: &Msg) {
        let f = Arc::new(m.frame());
        let reg = self.sh.reg.lock().unwrap();
        for c in reg.conns.values() {
            let _ = c.tx.send(f.clone());
        }
    }

    pub fn connected(&self, id: u64) -> bool {
        self.sh.reg.lock().unwrap().conns.contains_key(&id)
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        self.sh.stop.store(true, Ordering::Relaxed);
        let mut reg = self.sh.reg.lock().unwrap();
        for c in reg.conns.values() {
            let _ = c.sock.shutdown(std::net::Shutdown::Both);
        }
        reg.conns.clear();
        drop(reg);
        // Unblock accept().
        let _ = TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], self.sh.port)), Duration::from_millis(200));
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.into()).stack_size(256 * 1024).spawn(f);
}

fn send_beacon(sock: &UdpSocket, sh: &Shared, query: bool) {
    let b = Beacon { id: sh.me, port: sh.port, query }.encode();
    let _ = sock.send_to(&b, SocketAddrV4::new(GROUP, UDP_PORT));
    let _ = sock.send_to(&b, SocketAddrV4::new(Ipv4Addr::BROADCAST, UDP_PORT));
    for (_, bc) in interfaces() {
        let _ = sock.send_to(&b, SocketAddrV4::new(bc, UDP_PORT));
    }
    // Other Slates on this PC, even with no network at all.
    let _ = sock.send_to(&b, SocketAddrV4::new(Ipv4Addr::LOCALHOST, UDP_PORT));
}

fn listen_beacons(sh: Arc<Shared>, sock: UdpSocket, reply: UdpSocket) {
    let mut buf = [0u8; 64];
    // Several copies of one beacon arrive (multicast, broadcasts); answer once.
    let mut answered: HashMap<u64, Instant> = HashMap::new();
    while !sh.stop.load(Ordering::Relaxed) {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        let Some(b) = Beacon::decode(&buf[..n]) else { continue };
        if b.id == sh.me {
            continue;
        }
        if b.query && answered.get(&b.id).map_or(true, |t| t.elapsed() > Duration::from_secs(2)) {
            answered.insert(b.id, Instant::now());
            let me = Beacon { id: sh.me, port: sh.port, query: false }.encode();
            let _ = reply.send_to(&me, from);
        }
        heard(&sh, b, from);
    }
}

/// A beacon from `b.id` arrived from `from`: connect if not connected.
fn heard(sh: &Arc<Shared>, b: Beacon, from: SocketAddr) {
    if b.id == sh.me {
        return;
    }
    let addr = SocketAddr::new(from.ip(), b.port);
    {
        let mut reg = sh.reg.lock().unwrap();
        reg.addrs.insert(b.id, addr);
        if reg.conns.contains_key(&b.id) || reg.connecting.contains(&b.id) {
            return;
        }
        reg.connecting.insert(b.id);
    }
    let sh = sh.clone();
    spawn("slate-connect", move || {
        let r = TcpStream::connect_timeout(&addr, Duration::from_secs(3));
        sh.reg.lock().unwrap().connecting.remove(&b.id);
        if let Ok(s) = r {
            handshake(sh, s, true);
        }
    });
}

/// Exchanges hellos, registers the connection, then reads until it closes.
fn handshake(sh: Arc<Shared>, mut s: TcpStream, initiated: bool) {
    if sh.stop.load(Ordering::Relaxed) {
        return;
    }
    keepalive(&s);
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    if s.write_all(&sh.hello.frame()).is_err() {
        return;
    }
    let Ok(f) = read_frame(&mut s) else { return };
    let Some(Msg::Hello { id, protocol, name, pc, version, hub }) = Msg::decode(&f) else { return };
    if protocol != PROTOCOL || id == sh.me || id == 0 {
        return;
    }
    let _ = s.set_read_timeout(None);
    let serial = sh.serial.fetch_add(1, Ordering::Relaxed);
    let initiator = if initiated { sh.me } else { id };
    let (tx, rx) = mpsc::channel::<Arc<Vec<u8>>>();
    let Ok(wsock) = s.try_clone() else { return };
    let Ok(ksock) = s.try_clone() else { return };
    let fresh = {
        let mut reg = sh.reg.lock().unwrap();
        let keep_new = match reg.conns.get(&id) {
            None => true,
            // Both sides connected at once: on both PCs the connection
            // opened by the lower id wins.
            Some(old) => old.initiator != initiator && initiator == sh.me.min(id),
        };
        if !keep_new {
            let _ = s.shutdown(std::net::Shutdown::Both);
            return;
        }
        let old = reg.conns.insert(id, Conn { serial, initiator, tx, sock: ksock });
        if let Some(o) = &old {
            let _ = o.sock.shutdown(std::net::Shutdown::Both);
        }
        old.is_none()
    };
    // Writer: frames queued by the editor go out in order.
    {
        let mut w = wsock;
        spawn("slate-write", move || {
            for f in rx {
                if w.write_all(&f).is_err() {
                    let _ = w.shutdown(std::net::Shutdown::Both);
                    break;
                }
            }
        });
    }
    if fresh {
        sh.sink.post(Event::Up { id, name, pc, version, hub });
    }
    // Reader.
    loop {
        let Ok(f) = read_frame(&mut s) else { break };
        if let Some(msg) = Msg::decode(&f) {
            sh.sink.post(Event::Msg { from: id, msg });
        }
    }
    let gone = {
        let mut reg = sh.reg.lock().unwrap();
        // Only if this connection was not replaced by a newer one.
        if reg.conns.get(&id).map_or(false, |c| c.serial == serial) {
            reg.conns.remove(&id);
            true
        } else {
            false
        }
    };
    if gone && !sh.stop.load(Ordering::Relaxed) {
        sh.sink.post(Event::Down { id });
        // It may only be a hiccup: try again once if it is still announcing.
        let addr = sh.reg.lock().unwrap().addrs.get(&id).copied();
        if let Some(addr) = addr {
            let sh2 = sh.clone();
            spawn("slate-reconnect", move || {
                std::thread::sleep(Duration::from_secs(2));
                if sh2.stop.load(Ordering::Relaxed) {
                    return;
                }
                heard(&sh2, Beacon { id, port: addr.port(), query: false }, addr);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two instances in one process find each other over loopback and
    /// exchange a message.
    #[test]
    fn two_peers_connect_and_talk() {
        let (ta, ra) = mpsc::channel();
        let (tb, rb) = mpsc::channel();
        let a = Net::start(new_id(), "A".into(), "PC".into(), false, Sink::Channel(ta)).unwrap();
        let b = Net::start(new_id(), "B".into(), "PC".into(), false, Sink::Channel(tb)).unwrap();
        a.announce(true);
        b.announce(true);
        let wait_up = |r: &mpsc::Receiver<Event>, want: u64| loop {
            match r.recv_timeout(Duration::from_secs(10)).expect("peer did not come up") {
                Event::Up { id, .. } if id == want => break,
                _ => {}
            }
        };
        wait_up(&ra, b.me());
        wait_up(&rb, a.me());
        assert!(a.send(b.me(), &Msg::Join { sid: 7 }));
        loop {
            match rb.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Msg { from, msg: Msg::Join { sid: 7 } } => {
                    assert_eq!(from, a.me());
                    break;
                }
                _ => {}
            }
        }
        // One connection each way survives, however the race went.
        std::thread::sleep(Duration::from_millis(300));
        assert!(a.connected(b.me()) && b.connected(a.me()));
        drop(b);
        loop {
            match ra.recv_timeout(Duration::from_secs(10)).expect("down not seen") {
                Event::Down { id } => {
                    assert_ne!(id, 0);
                    break;
                }
                _ => {}
            }
        }
    }
}
