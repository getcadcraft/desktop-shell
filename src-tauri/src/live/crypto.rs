// SPDX-License-Identifier: Apache-2.0
//! Studio Live: the handshake and the frame encryption (spec docs/STUDIO-LIVE-SPEC.md § 5, § 6, § 9; the byte layout is
//! docs/LIVE-CRYPTO.md in the site repo). Owner: 0.5 session `live-relay`. The JS end is `assets/js/live-crypto.js`;
//! both check the same vectors (`tools/live-crypto/vectors.json` in the site repo, written by
//! `/var/www/CADCRAFT/relay/crypto-check`).
//!
//! This module knows nothing of sockets or MessagePack. The session server carries the handshake's four messages in
//! `pake` envelopes and passes every later frame through `Session::seal` / `Session::open`.
//!
//! Handshake (joiner = SPAKE2 side A, host = side B; RustCrypto `spake2`, Ed25519 group):
//! 1. joiner → host: A message (33 bytes, `'A'` ‖ point)
//! 2. host → joiner: B message (33 bytes, `'B'` ‖ point)
//! 3. joiner → host: joiner confirmation (32 bytes, HMAC)
//! 4. host → joiner: host confirmation (32 bytes), sent only after 3 checked out
//!
//! The joiner proves the code first, so a wrong code is always the host's to count (3 in a row rotate the code), and a
//! guesser learns nothing from the host. A handshake that stops before message 3 also counts as a wrong try: that is
//! the server's rule, not this module's.
//!
//! Frames: `header (11) ‖ ChaCha20-Poly1305(key, nonce, body, aad = header)`. Header = version 1, kind (0 data,
//! 1 rekey), epoch, counter (u64 big-endian). Nonce = direction (`'J'` joiner → host, `'H'` host → joiner) ‖ epoch ‖
//! 0 ‖ 0 ‖ counter. Each direction has its own key and counter; a counter that is not exactly the next one, a wrong
//! epoch or a failed tag is an error and the server closes the connection.
//!
//! Key rotation: `Session::rekey` (host only) sends a rekey frame under the old key and moves both directions to the
//! next key (`HKDF(old key, "next")`); the joiner follows when it opens that frame. The host calls it on every
//! remaining connection when it removes someone, so frames recorded before then can't be opened with later keys.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand_core::{CryptoRng, RngCore};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

/// Version byte of every frame header, and the label in every derived key.
pub const VERSION: u8 = 1;
const LABEL: &[u8] = b"cadcraft-live/1";
pub const PAKE_LEN: usize = 33;
pub const CONFIRM_LEN: usize = 32;
pub const HEADER_LEN: usize = 11;
pub const TAG_LEN: usize = 16;
const KIND_DATA: u8 = 0;
const KIND_REKEY: u8 = 1;
const DIR_JOINER: u8 = b'J';
const DIR_HOST: u8 = b'H';

/// Crockford base32: no I, L, O or U.
pub const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A short code `TT-SSSSS` as typed: case, spaces and the dash don't matter; O reads as 0, I and L as 1. Returns the
/// public tag and the secret (the 5 normalized characters, the SPAKE2 password). The context is `lan:<tag>`.
pub fn parse_code(code: &str) -> Option<(String, Vec<u8>)> {
    let mut out = Vec::with_capacity(7);
    for ch in code.chars() {
        let c = match ch.to_ascii_uppercase() {
            ' ' | '-' => continue,
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        if !c.is_ascii() || !CODE_ALPHABET.contains(&(c as u8)) {
            return None;
        }
        out.push(c as u8);
    }
    if out.len() != 7 {
        return None;
    }
    Some((String::from_utf8(out[..2].to_vec()).ok()?, out[2..].to_vec()))
}

/// A new short code from the system's random source, e.g. `K7-Q4M2X`.
pub fn new_code() -> String {
    let mut b = [0u8; 7];
    rand_core::OsRng.fill_bytes(&mut b);
    let c: Vec<char> = b.iter().map(|x| CODE_ALPHABET[(x & 31) as usize] as char).collect();
    format!("{}{}-{}", c[0], c[1], c[2..].iter().collect::<String>())
}

/// An invite link's fragment (base64url, no padding) → the secret bytes (16–32 of them). The context is
/// `relay:<room>`.
pub fn parse_link_secret(fragment: &str) -> Option<Vec<u8>> {
    let f = fragment.strip_prefix('#').unwrap_or(fragment);
    let mut bits: u32 = 0;
    let mut n = 0;
    let mut out = vec![];
    for ch in f.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        } as u32;
        bits = (bits << 6) | v;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    // Leftover bits must be zero (one spelling per secret); 16–32 bytes.
    if bits != 0 || f.len() % 4 == 1 || !(16..=32).contains(&out.len()) {
        return None;
    }
    Some(out)
}

/// The LAN tag of an invite-link room (LV.26): 8 Crockford characters from SHA-256("cadcraft-live/1 room " ‖ room).
/// A host with an open relay room also advertises `_ccstudio._tcp` under this tag, so a joiner holding the link finds it
/// on the same network, while nobody else on the network learns the room id.
pub fn room_tag(room: &str) -> String {
    use sha2::Digest;
    let mut h = Sha256::new();
    h.update(LABEL);
    h.update(b" room ");
    h.update(room.as_bytes());
    let d = h.finalize();
    let bits = d[..5].iter().fold(0u64, |acc, x| (acc << 8) | *x as u64);
    (0..8).map(|i| CODE_ALPHABET[((bits >> (35 - 5 * i)) & 31) as usize] as char).collect()
}

/// A new 128-bit invite secret as a link fragment (22 characters).
pub fn new_link_secret() -> String {
    let mut b = [0u8; 16];
    rand_core::OsRng.fill_bytes(&mut b);
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::new();
    for chunk in b.chunks(3) {
        let v = chunk.iter().fold(0u32, |acc, x| (acc << 8) | *x as u32) << (8 * (3 - chunk.len()));
        for i in 0..(chunk.len() + 1) {
            s.push(A[((v >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    s
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// A handshake message of the wrong length or side, or a point that isn't on the curve.
    BadMessage,
    /// The confirmation didn't match: the two sides used different codes (or someone tampered).
    WrongCode,
    /// A frame that is too short, has another version or kind, or fails its tag.
    BadFrame,
    /// A frame whose counter or epoch isn't the next one (replayed, reordered or dropped).
    Order,
    /// Only the host may rotate keys; the joiner refuses a rekey it didn't expect and never sends one.
    Role,
    /// 255 rotations or 2^63 frames in one direction: end the connection and join again.
    Exhausted,
    /// The handshake was used out of turn.
    State,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CryptoError::BadMessage => "bad_message",
            CryptoError::WrongCode => "wrong_code",
            CryptoError::BadFrame => "bad_frame",
            CryptoError::Order => "order",
            CryptoError::Role => "role",
            CryptoError::Exhausted => "exhausted",
            CryptoError::State => "state",
        })
    }
}
impl std::error::Error for CryptoError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Joiner,
    Host,
}

/// The two SPAKE2 identities: they bind the session's context ("lan:<tag>" or "relay:<room>"), so a handshake
/// recorded in one session is useless in another.
fn identities(context: &str) -> (Vec<u8>, Vec<u8>) {
    let mut a = LABEL.to_vec();
    a.extend_from_slice(b" joiner ");
    a.extend_from_slice(context.as_bytes());
    let mut b = LABEL.to_vec();
    b.extend_from_slice(b" host ");
    b.extend_from_slice(context.as_bytes());
    (a, b)
}

fn derive(ikm: &[u8], info: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(Some(LABEL), ikm).expand(info.as_bytes(), &mut out).expect("32 bytes");
    out
}

fn confirm_mac(key: &[u8; 32], who: &[u8], a: &[u8], b: &[u8]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key length");
    m.update(who);
    m.update(a);
    m.update(b);
    m.finalize().into_bytes().into()
}

/// Constant-time comparison of two confirmations.
fn same(x: &[u8], y: &[u8]) -> bool {
    x.len() == y.len() && x.iter().zip(y).fold(0u8, |acc, (p, q)| acc | (p ^ q)) == 0
}

/// Everything the four handshake messages agree on.
struct Agreed {
    a: Vec<u8>,
    b: Vec<u8>,
    k: Vec<u8>,
}

impl Agreed {
    fn confirm(&self, role: Role) -> [u8; 32] {
        let (info, who) = match role {
            Role::Joiner => ("confirm joiner", b"joiner".as_slice()),
            Role::Host => ("confirm host", b"host".as_slice()),
        };
        confirm_mac(&derive(&self.k, info), who, &self.a, &self.b)
    }
    fn session(&self, role: Role) -> Session {
        let j2h = derive(&self.k, "frames j2h");
        let h2j = derive(&self.k, "frames h2j");
        let (send, recv) = match role {
            Role::Joiner => (j2h, h2j),
            Role::Host => (h2j, j2h),
        };
        Session {
            role,
            send: Direction::new(send, if role == Role::Joiner { DIR_JOINER } else { DIR_HOST }),
            recv: Direction::new(recv, if role == Role::Joiner { DIR_HOST } else { DIR_JOINER }),
            recv_max: 0,
            resume: derive(&self.k, "resume"),
        }
    }
}

/// The joiner's side: `start` → send its message; `receive(host's)` → send the confirmation it returns;
/// `finish(host's confirmation)` → the session.
pub struct JoinerHandshake {
    spake: Option<Spake2<Ed25519Group>>,
    a: Vec<u8>,
    agreed: Option<Agreed>,
}

impl JoinerHandshake {
    pub fn start(secret: &[u8], context: &str) -> (Self, Vec<u8>) {
        Self::start_with_rng(secret, context, rand_core::OsRng)
    }
    /// The RNG gives the SPAKE2 scalar (64 bytes, reduced): tests and vectors only pass a fixed one.
    pub fn start_with_rng(secret: &[u8], context: &str, rng: impl CryptoRng + RngCore) -> (Self, Vec<u8>) {
        let (ia, ib) = identities(context);
        let (s, msg) = Spake2::<Ed25519Group>::start_a_with_rng(
            &Password::new(secret),
            &Identity::new(&ia),
            &Identity::new(&ib),
            rng,
        );
        (JoinerHandshake { spake: Some(s), a: msg.clone(), agreed: None }, msg)
    }
    /// Takes the host's B message; returns the joiner's confirmation to send.
    pub fn receive(&mut self, host_msg: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let s = self.spake.take().ok_or(CryptoError::State)?;
        if host_msg.len() != PAKE_LEN || host_msg[0] != b'B' {
            return Err(CryptoError::BadMessage);
        }
        let k = s.finish(host_msg).map_err(|_| CryptoError::BadMessage)?;
        let agreed = Agreed { a: self.a.clone(), b: host_msg.to_vec(), k };
        let c = agreed.confirm(Role::Joiner).to_vec();
        self.agreed = Some(agreed);
        Ok(c)
    }
    /// Checks the host's confirmation. `WrongCode` here means the code was wrong (or the host is not who it says).
    pub fn finish(self, host_confirm: &[u8]) -> Result<Session, CryptoError> {
        let agreed = self.agreed.ok_or(CryptoError::State)?;
        if !same(host_confirm, &agreed.confirm(Role::Host)) {
            return Err(CryptoError::WrongCode);
        }
        Ok(agreed.session(Role::Joiner))
    }
}

/// The host's side, one per connection: `receive(joiner's)` → send the B message it returns;
/// `finish(joiner's confirmation)` → the host's confirmation to send, and the session.
pub struct HostHandshake {
    secret: Vec<u8>,
    context: String,
    agreed: Option<Agreed>,
}

impl HostHandshake {
    pub fn new(secret: &[u8], context: &str) -> Self {
        HostHandshake { secret: secret.to_vec(), context: context.into(), agreed: None }
    }
    pub fn receive(&mut self, joiner_msg: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.receive_with_rng(joiner_msg, rand_core::OsRng)
    }
    pub fn receive_with_rng(&mut self, joiner_msg: &[u8], rng: impl CryptoRng + RngCore) -> Result<Vec<u8>, CryptoError> {
        if self.agreed.is_some() {
            return Err(CryptoError::State);
        }
        if joiner_msg.len() != PAKE_LEN || joiner_msg[0] != b'A' {
            return Err(CryptoError::BadMessage);
        }
        let (ia, ib) = identities(&self.context);
        let (s, b) = Spake2::<Ed25519Group>::start_b_with_rng(
            &Password::new(&self.secret),
            &Identity::new(&ia),
            &Identity::new(&ib),
            rng,
        );
        let k = s.finish(joiner_msg).map_err(|_| CryptoError::BadMessage)?;
        self.agreed = Some(Agreed { a: joiner_msg.to_vec(), b: b.clone(), k });
        Ok(b)
    }
    /// Checks the joiner's confirmation: `WrongCode` = a wrong try to count. On success, send the returned
    /// confirmation and keep the session.
    pub fn finish(self, joiner_confirm: &[u8]) -> Result<(Vec<u8>, Session), CryptoError> {
        let agreed = self.agreed.ok_or(CryptoError::State)?;
        if !same(joiner_confirm, &agreed.confirm(Role::Joiner)) {
            return Err(CryptoError::WrongCode);
        }
        Ok((agreed.confirm(Role::Host).to_vec(), agreed.session(Role::Host)))
    }
}

struct Direction {
    key: [u8; 32],
    dir: u8,
    epoch: u8,
    counter: u64,
}

impl Direction {
    fn new(key: [u8; 32], dir: u8) -> Self {
        Direction { key, dir, epoch: 0, counter: 0 }
    }
    fn nonce(&self) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[0] = self.dir;
        n[1] = self.epoch;
        n[4..].copy_from_slice(&self.counter.to_be_bytes());
        n
    }
    fn header(&self, kind: u8) -> [u8; HEADER_LEN] {
        let mut h = [0u8; HEADER_LEN];
        h[0] = VERSION;
        h[1] = kind;
        h[2] = self.epoch;
        h[3..].copy_from_slice(&self.counter.to_be_bytes());
        h
    }
    fn next(&mut self) -> Result<(), CryptoError> {
        if self.epoch == u8::MAX {
            return Err(CryptoError::Exhausted);
        }
        self.key = derive(&self.key, "next");
        self.epoch += 1;
        self.counter = 0;
        Ok(())
    }
}

impl Drop for Direction {
    fn drop(&mut self) {
        self.key = [0u8; 32];
    }
}

/// What `open` found in a frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Opened {
    Data(Vec<u8>),
    /// The host moved to the next keys; the joiner already followed. Nothing to hand on.
    Rekeyed,
}

/// One connection's keys after the handshake.
pub struct Session {
    role: Role,
    send: Direction,
    recv: Direction,
    /// Host only: the newest epoch it announced. The joiner's frames may come under any epoch from `recv.epoch` up
    /// to this one (it may have sent some under a middle key before the next rekey frame reached it).
    recv_max: u8,
    resume: [u8; 32],
}

impl Session {
    pub fn role(&self) -> Role {
        self.role
    }
    /// The send direction's epoch (how many rotations so far).
    pub fn epoch(&self) -> u8 {
        self.send.epoch
    }

    fn seal_kind(&mut self, kind: u8, body: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if self.send.counter >= 1 << 63 {
            return Err(CryptoError::Exhausted);
        }
        let header = self.send.header(kind);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.send.key));
        let ct = cipher
            .encrypt(Nonce::from_slice(&self.send.nonce()), Payload { msg: body, aad: &header })
            .map_err(|_| CryptoError::BadFrame)?;
        self.send.counter += 1;
        let mut out = Vec::with_capacity(HEADER_LEN + ct.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Encrypts one frame body (a MessagePack envelope).
    pub fn seal(&mut self, body: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.seal_kind(KIND_DATA, body)
    }

    /// Host only: tells the joiner to move to the next keys, and moves. Send the returned frame before any other.
    pub fn rekey(&mut self) -> Result<Vec<u8>, CryptoError> {
        if self.role != Role::Host {
            return Err(CryptoError::Role);
        }
        if self.recv_max == u8::MAX {
            return Err(CryptoError::Exhausted);
        }
        self.recv_max += 1;
        let frame = self.seal_kind(KIND_REKEY, &[])?;
        self.send.next()?;
        Ok(frame)
    }

    /// Decrypts one frame. Any error means: close the connection.
    pub fn open(&mut self, frame: &[u8]) -> Result<Opened, CryptoError> {
        if frame.len() < HEADER_LEN + TAG_LEN || frame[0] != VERSION || frame[1] > KIND_REKEY {
            return Err(CryptoError::BadFrame);
        }
        let kind = frame[1];
        let epoch = frame[2];
        let mut cb = [0u8; 8];
        cb.copy_from_slice(&frame[3..HEADER_LEN]);
        let counter = u64::from_be_bytes(cb);
        if kind == KIND_REKEY && self.role == Role::Host {
            return Err(CryptoError::Role);
        }
        // Host: the joiner's first frame under a newer announced key ends the older ones (its counter starts at 0).
        let ahead = epoch > self.recv.epoch && epoch <= self.recv_max;
        let mut d = Direction::new(self.recv.key, self.recv.dir);
        d.epoch = self.recv.epoch;
        d.counter = self.recv.counter;
        if ahead {
            while d.epoch < epoch {
                d.next()?;
            }
        }
        if epoch != d.epoch || counter != d.counter {
            return Err(CryptoError::Order);
        }
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&d.key));
        let body = cipher
            .decrypt(Nonce::from_slice(&d.nonce()), Payload { msg: &frame[HEADER_LEN..], aad: &frame[..HEADER_LEN] })
            .map_err(|_| CryptoError::BadFrame)?;
        d.counter += 1;
        self.recv = d;
        if kind == KIND_REKEY {
            if !body.is_empty() {
                return Err(CryptoError::BadFrame);
            }
            self.recv.next()?;
            self.send.next()?;
            self.recv_max = self.recv.epoch;
            return Ok(Opened::Rekeyed);
        }
        Ok(Opened::Data(body))
    }

    /// A 32-byte secret for one purpose (`label`), the same on both ends and never sent: e.g. a reconnect ticket for
    /// host handoff (LV.17). Not changed by rekeys.
    pub fn exporter(&self, label: &str) -> [u8; 32] {
        derive(&self.resume, label)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.resume = [0u8; 32];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(js: &[u8], hs: &[u8], ctx: &str) -> Result<(Session, Session), CryptoError> {
        let (mut j, a) = JoinerHandshake::start(js, ctx);
        let mut h = HostHandshake::new(hs, ctx);
        let b = h.receive(&a)?;
        let cj = j.receive(&b)?;
        let (ch, host) = h.finish(&cj)?;
        let joiner = j.finish(&ch)?;
        Ok((joiner, host))
    }

    #[test]
    fn codes_and_links() {
        assert_eq!(parse_code("k7-q4m2x"), Some(("K7".into(), b"Q4M2X".to_vec())));
        assert_eq!(parse_code(" K7 QOMIL "), Some(("K7".into(), b"Q0M11".to_vec())));
        assert_eq!(parse_code("K7-Q4M2U"), None);
        assert_eq!(parse_code("K7-Q4M2"), None);
        for _ in 0..50 {
            let c = new_code();
            assert_eq!(c.len(), 8);
            assert!(parse_code(&c).is_some());
            let l = new_link_secret();
            assert_eq!(l.len(), 22);
            assert_eq!(parse_link_secret(&l).map(|v| v.len()), Some(16));
        }
        assert_eq!(parse_link_secret("#paWlpaWlpaWlpaWlpaWlpQ"), Some(vec![0xa5; 16]));
        assert_eq!(parse_link_secret("paWlpaWlpaWlpaWlpaWlpR"), None); // stray low bits
        assert_eq!(parse_link_secret("paWl"), None); // too short
        assert_eq!(parse_link_secret("paWlpaWlpaWlpaWlpaWlpQ=="), None); // padding
        let t = room_tag("K7QX9M2B");
        assert_eq!(t.len(), 8);
        assert!(t.bytes().all(|c| CODE_ALPHABET.contains(&c)));
        assert_ne!(t, room_tag("K7QX9M2C"));
    }

    #[test]
    fn handshake_and_frames() {
        let (mut j, mut h) = pair(b"Q4M2X", b"Q4M2X", "lan:K7").unwrap();
        let f = j.seal(b"hello").unwrap();
        assert_eq!(h.open(&f).unwrap(), Opened::Data(b"hello".to_vec()));
        let g = h.seal(b"welcome").unwrap();
        assert_eq!(j.open(&g).unwrap(), Opened::Data(b"welcome".to_vec()));
        assert_eq!(j.exporter("ticket"), h.exporter("ticket"));
    }

    #[test]
    fn wrong_code_is_the_hosts_to_count() {
        assert_eq!(pair(b"Q4M2X", b"Q4M2Y", "lan:K7").err(), Some(CryptoError::WrongCode));
        // Same code, other session: the identities differ, so it fails too.
        let (mut j, a) = JoinerHandshake::start(b"Q4M2X", "lan:K7");
        let mut h = HostHandshake::new(b"Q4M2X", "lan:K8");
        let b = h.receive(&a).unwrap();
        let cj = j.receive(&b).unwrap();
        assert_eq!(h.finish(&cj).err(), Some(CryptoError::WrongCode));
    }

    #[test]
    fn replay_reorder_tamper() {
        let (mut j, mut h) = pair(b"s", b"s", "relay:ROOM").unwrap();
        let f1 = j.seal(b"1").unwrap();
        let f2 = j.seal(b"2").unwrap();
        assert_eq!(h.open(&f2).err(), Some(CryptoError::Order));
        h.open(&f1).unwrap();
        assert_eq!(h.open(&f1).err(), Some(CryptoError::Order));
        let mut f3 = j.seal(b"3").unwrap();
        let _ = h.open(&f2).unwrap();
        let last = f3.len() - 1;
        f3[last] ^= 1;
        assert_eq!(h.open(&f3).err(), Some(CryptoError::BadFrame));
        // A host's own frame sent back to it (reflection) is refused: other direction, other key.
        let mut j2 = pair(b"s", b"s", "x").unwrap();
        let echo = j2.1.seal(b"x").unwrap();
        assert!(j2.1.open(&echo).is_err());
    }

    #[test]
    fn rekey_both_directions_with_frames_in_flight() {
        let (mut j, mut h) = pair(b"s", b"s", "lan:AB").unwrap();
        let before = j.seal(b"in flight").unwrap();
        let r = h.rekey().unwrap();
        assert_eq!(h.open(&before).unwrap(), Opened::Data(b"in flight".to_vec()));
        assert_eq!(j.open(&r).unwrap(), Opened::Rekeyed);
        let after = j.seal(b"new key").unwrap();
        assert_eq!(after[2], 1);
        assert_eq!(h.open(&after).unwrap(), Opened::Data(b"new key".to_vec()));
        let back = h.seal(b"ok").unwrap();
        assert_eq!(j.open(&back).unwrap(), Opened::Data(b"ok".to_vec()));
        // An old-epoch frame after the switch is refused.
        assert_eq!(h.open(&before).err(), Some(CryptoError::Order));
        // Joiners never rotate.
        assert_eq!(j.rekey().err(), Some(CryptoError::Role));
    }

    #[test]
    fn joiner_frame_under_a_middle_key() {
        let (mut j, mut h) = pair(b"s", b"s", "lan:AB").unwrap();
        let r1 = h.rekey().unwrap();
        assert_eq!(j.open(&r1).unwrap(), Opened::Rekeyed);
        let e1 = j.seal(b"e1").unwrap();
        let r2 = h.rekey().unwrap();
        assert_eq!(h.open(&e1).unwrap(), Opened::Data(b"e1".to_vec()));
        assert_eq!(j.open(&r2).unwrap(), Opened::Rekeyed);
        let e2 = j.seal(b"e2").unwrap();
        assert_eq!(h.open(&e2).unwrap(), Opened::Data(b"e2".to_vec()));
        // An epoch the host never announced is refused.
        let mut fake = j.seal(b"x").unwrap();
        fake[2] = 9;
        assert_eq!(h.open(&fake).err(), Some(CryptoError::Order));
    }

    #[test]
    fn two_rekeys_before_the_joiner_answers() {
        let (mut j, mut h) = pair(b"s", b"s", "lan:AB").unwrap();
        let r1 = h.rekey().unwrap();
        let r2 = h.rekey().unwrap();
        assert_eq!(j.open(&r1).unwrap(), Opened::Rekeyed);
        assert_eq!(j.open(&r2).unwrap(), Opened::Rekeyed);
        let f = j.seal(b"e2").unwrap();
        assert_eq!(f[2], 2);
        assert_eq!(h.open(&f).unwrap(), Opened::Data(b"e2".to_vec()));
    }
}
