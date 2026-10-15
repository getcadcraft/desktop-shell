// SPDX-License-Identifier: Apache-2.0
//! The session server's core (spec docs/STUDIO-LIVE-SPEC.md § 5, § 5a, § 6, § 7, § 11; messages docs/LIVE-PROTOCOL.md).
//!
//! Pure state machine: no sockets, no clock of its own, no threads. `net.rs` feeds it frames and the time and carries
//! out what it returns, from one task off the UI thread. Tests drive it directly.
//!
//! What it decides:
//! - who gets in: the PAKE handshake against the invite secrets (short codes, link keys), wrong-try counting and code
//!   rotation; then hello → waiting → the host approves (a rejoin with the resume token skips approval); roles;
//! - the order: every edit gets the next `seq` or a `refuse`, all-or-nothing;
//! - property writes commit last-writer-wins (refused only on a deleted object, or when an undo's condition fails);
//!   structural ops commit only if every key they name is still at the version they were based on;
//! - holds (hints that stop others' structural ops and guarded reads: a held layer or area, LV.9), locks (host only, stop
//!   every op, the host's too);
//! - the validator (the app adapter's `validate`, run by a replica that holds exactly the committed state) before
//!   every commit;
//! - presence fan-out, pings every 2 s, 3 missed = gone, flood control, limits;
//! - catch-up from the log or a snapshot, `saved`, `end`.

use super::crypto;
use super::msg::{get, get_arr, get_str, get_u64, map, Envelope, PROTOCOL};
use rmpv::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

pub type ConnId = u64;
/// Milliseconds on the caller's monotonic clock.
pub type Ms = u64;

pub const PING_EVERY: Ms = 2_000;
pub const GONE_AFTER: Ms = 6_000; // 3 missed pings
/// Presence fan-out tick. Each peer's newest presence is forwarded within one tick, so a peer moving at 30 a second
/// is seen at 30 a second (a 33 ms tick beats against the senders' clocks and drops to about 20).
pub const PRESENCE_EVERY: Ms = 16;
pub const HOLD_TTL: Ms = 30_000;
pub const SNAPSHOT_CHUNK: usize = 256 * 1024;
/// Key versions per `revs` message sent before a snapshot (keeps each frame well under the relay's 2 MB).
pub const REVS_CHUNK: usize = 10_000;
pub const MAX_EDIT_BYTES: usize = 1024 * 1024;
pub const RATE_PER_SEC: f64 = 100.0;
pub const BACKLOG_WARN: usize = 200;
pub const BACKLOG_REMOVE: usize = 1000;
/// How long a dropped client's place (id, prefix, holds) is kept for a rejoin.
pub const REJOIN_WINDOW: Ms = 10 * 60_000;
/// A joiner that sent handshake message 1 must prove the code (message 3) within this, or it counts as a wrong try.
pub const HANDSHAKE_TIMEOUT: Ms = 10_000;
pub const CODE_TTL: Ms = 10 * 60_000;
pub const WRONG_TRIES: u32 = 3;
/// A rejoin ticket after a host handoff lasts as long as the new session (§ 10).
const TICKET_TTL: Ms = Ms::MAX / 4;
/// An assistant's one-time key (LV.18): asked for by its person, used at once by the assistant's own connection.
pub const ASSISTANT_KEY_TTL: Ms = 60_000;
/// Assistants per person at a time (connected, waiting, or keys not used yet).
pub const MAX_ASSISTANTS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Host,
    Editor,
    Viewer,
    Assistant,
    /// The host's validator: holds exactly the committed state, answers `validate` and `snapreq`. Never a person.
    Replica,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Host => "host",
            Role::Editor => "editor",
            Role::Viewer => "viewer",
            Role::Assistant => "assistant",
            Role::Replica => "replica",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        Some(match s {
            "host" => Role::Host,
            "editor" | "edit" => Role::Editor,
            "viewer" | "view" => Role::Viewer,
            "assistant" => Role::Assistant,
            "replica" => Role::Replica,
            _ => return None,
        })
    }
    fn can_edit(self) -> bool {
        matches!(self, Role::Host | Role::Editor | Role::Assistant)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Waiting,
    Active,
    Gone,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub app: String,
    pub schema: u32,
    pub max_clients: usize,
    /// How many commits the log keeps for rejoins and same-file joins; older ones mean a snapshot.
    pub log_keep: usize,
    /// Spike and test servers: approve every joiner as an editor without asking the host.
    pub auto_approve: bool,
    /// Secret a privileged connection presents to become the host or the replica (a test harness; it may then skip the
    /// handshake). Empty = only in-process connections (`attach_local`) can. Studio never sets it.
    pub local_token: String,
    /// Test servers only, never Studio: network connections skip the handshake and frame encryption.
    pub plain: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            app: String::new(),
            schema: 1,
            max_clients: 20,
            log_keep: 10_000,
            auto_approve: false,
            local_token: String::new(),
            plain: false,
        }
    }
}

/// What the session wants done. `net.rs` carries these out in order.
pub enum Out {
    To(ConnId, Envelope),
    /// Close after sending what is queued for that connection.
    Close(ConnId, String),
    /// The handshake succeeded: seal and open every later frame of this connection with this session.
    Secure(ConnId, Box<crypto::Session>),
    /// Rotate this connection's frame keys (someone was removed): send the rekey frame before anything else.
    Rekey(ConnId),
}

impl std::fmt::Debug for Out {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Out::To(c, e) => write!(f, "To({c}, {})", e.t),
            Out::Close(c, why) => write!(f, "Close({c}, {why})"),
            Out::Secure(c, _) => write!(f, "Secure({c})"),
            Out::Rekey(c) => write!(f, "Rekey({c})"),
        }
    }
}

#[derive(Debug, Clone)]
struct Client {
    id: u32,
    name: String,
    device: String,
    via: String,
    kid: String,
    proof: Value,
    /// Signed-in name (LV.19): the site's assertion and, with SI.8, a 64 × 64 picture; the host checks them.
    assertion: String,
    picture: Value,
    /// The connection's assertion audience (base64url of its crypto session's `exporter("assertion")`), for the host's
    /// check of `assertion`; empty without a handshake.
    aud: String,
    /// The rejoin ticket for a host handoff (§ 10): the connection's `exporter("ticket")`, or for an in-process client
    /// (the old host) a random one handed over in its own `moved`. Empty until needed.
    ticket: Vec<u8>,
    role: Role,
    prefix: String,
    token: String,
    conn: Option<ConnId>,
    state: State,
    last_seen: Ms,
    gone_at: Ms,
    /// Highest edit counter already decided (committed or refused): a re-sent edit at or below it is ignored.
    last_n: u64,
    presence: Value,
    presence_dirty: bool,
    bucket: f64,
    bucket_at: Ms,
    backlog: VecDeque<Envelope>,
    warned: bool,
    /// The file the joiner has (its hash), for the same-file catch-up.
    hash: String,
    /// The last refusals sent (n → refuse body): a refusal lost in a dropped connection is sent again on rejoin.
    refusals: VecDeque<(u64, Value)>,
    /// An assistant client's person (LV.18), 0 for people.
    of: u32,
}

/// A connection before and after it belongs to a client.
struct Conn {
    client: Option<u32>,
    /// In-process (the host's window, the replica): no handshake, may claim host / replica.
    trusted: bool,
    /// Handshake done (or not needed): envelopes other than `pake` are accepted.
    secure: bool,
    /// Frames on this connection are encrypted (so a removal rotates its keys).
    sealed: bool,
    hs: Option<Handshake>,
    via: String,
    kid: String,
    opened: Ms,
    /// Set where the handshake finishes (see `Client::aud`, `Client::ticket`).
    aud: String,
    ticket: Vec<u8>,
    /// The handshake used an assistant's key (LV.18): its person.
    of: u32,
}

struct Handshake {
    kid: String,
    started: Ms,
    host: Option<crypto::HostHandshake>,
}

#[derive(Debug, Clone, PartialEq)]
enum KeyKind {
    Code(String),
    Link,
    /// A person's rejoin ticket (kid `h:<client id>`, context `rejoin:<session id>`): their way back after a drop once the code
    /// or link they came with has expired, and to the new host after a handoff.
    Ticket,
    /// An assistant's one-time key (kid `a:<person>:<random>`, context `assistant:<session id>`, LV.18): the assistant connected
    /// to that person's window joins as its own client, role `assistant`, approved by the host (the host's own: at once).
    Assistant(u32),
}

/// Host handoff (spec § 10): the session stops committing from `handover` until everyone has moved.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Handover {
    None,
    /// The state went to this client; waiting for its `moved`.
    Waiting(u32),
    /// `moved` went out: this session only winds down.
    Done,
}

/// An invite secret the server accepts: a short code's or an invite link's.
struct Key {
    kind: KeyKind,
    secret: Vec<u8>,
    context: String,
    one_time: bool,
    ttl: Ms,
    expires: Ms,
    wrong: u32,
}

#[derive(Debug, Clone)]
pub struct Commit {
    pub q: u64,
    pub c: u32,
    pub n: u64,
    pub label: String,
    pub ops: Value,
    /// The edit's touched keys without their versions (`{k, m, f?}`): clients track key versions from them.
    pub touched: Value,
}

/// One entry of an edit's `touched` list (docs/LIVE-PROTOCOL.md § 6.2).
#[derive(Debug, Clone)]
struct Touch {
    k: String,
    f: Option<String>,
    r: Option<u64>,
    m: char,
    /// A guarded read (`g`, LV.9 layer and area holds): an `r` that someone else's hold on `k` refuses, as it does `s` and `d`.
    g: bool,
}

#[derive(Debug, Clone)]
struct Queued {
    c: u32,
    n: u64,
    label: String,
    ops: Value,
    touched: Vec<Touch>,
}

#[derive(Default)]
struct KeyState {
    /// seq of the last commit that wrote this key in any way (props included).
    rev: u64,
    /// who made it.
    by: u32,
    deleted: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub commits: u64,
    pub refused: u64,
    pub presence_out: u64,
    pub removed_flood: u64,
    pub wrong_tries: u64,
    pub rotations: u64,
}

pub struct Session {
    pub id: String,
    cfg: Config,
    clients: BTreeMap<u32, Client>,
    conns: HashMap<ConnId, Conn>,
    next_client: u32,
    seq: u64,
    log: VecDeque<Commit>,
    keys: HashMap<String, KeyState>,
    props: HashMap<String, (u64, u32)>,
    /// seq of the last structural commit (create / delete / structural write) per key.
    struct_rev: HashMap<String, u64>,
    holds: BTreeMap<String, (u32, Ms)>,
    locks: BTreeSet<String>,
    invites: BTreeMap<String, Key>,
    queue: VecDeque<Queued>,
    validating: Option<Queued>,
    replica: Option<u32>,
    saved_q: u64,
    saved_hash: String,
    /// Joiners waiting for a snapshot, served one at a time.
    snap_wait: VecDeque<u32>,
    snap_busy: Option<u32>,
    last_presence: Ms,
    last_ping: Ms,
    ended: bool,
    handover: Handover,
    /// Codes carried over from the old host (tags), told to the new host's window when it is in.
    carried: Vec<String>,
    /// The caller's clock at the last frame or tick (codes' time left, for a handoff's state).
    now: Ms,
    pub stats: Stats,
}

fn rand_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("os randomness");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Client-id prefixes for new object ids: short, never reused in a session.
fn prefix_for(id: u32) -> String {
    const A: &[u8] = b"abcdefghjkmnpqrstuvwxyz";
    let mut n = id as usize;
    let mut s = String::new();
    loop {
        s.insert(0, A[n % A.len()] as char);
        n /= A.len();
        if n == 0 {
            break;
        }
    }
    s
}

/// base64url without padding (the assertion audience, LIVE-CRYPTO.md § Signed-in names).
fn b64url(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::with_capacity(b.len() * 4 / 3 + 3);
    for chunk in b.chunks(3) {
        let v = chunk.iter().fold(0u32, |acc, x| (acc << 8) | *x as u32) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            s.push(A[((v >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    s
}

/// Equal byte strings, compared in constant time (an assistant's key in hello, LV.18).
fn same_bytes(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn ticket_kid(c: u32) -> String {
    format!("h:{c}")
}

/// Handoff state goes in chunks no bigger than this: the relay refuses frames over 2 MB (docs/LIVE-RELAY.md).
pub const HANDOVER_CHUNK: usize = 256 * 1024;

fn strs(v: &[String]) -> Value {
    Value::Array(v.iter().map(|s| Value::from(s.as_str())).collect())
}

fn prop_key(k: &str, f: &str) -> String {
    format!("{k}\u{0}{f}")
}

fn holds_change(set: &[(String, u32)], clear: &[String]) -> Envelope {
    let s: Vec<(Value, Value)> = set.iter().map(|(k, c)| (Value::from(k.as_str()), Value::from(*c))).collect();
    Envelope::new("holds").body(map(vec![("set", Value::Map(s)), ("clear", strs(clear))]))
}

fn commit_env(cm: &Commit) -> Envelope {
    let b = map(vec![
        ("c", cm.c.into()),
        ("n", cm.n.into()),
        ("label", cm.label.clone().into()),
        ("ops", cm.ops.clone()),
        ("touched", cm.touched.clone()),
    ]);
    let mut env = Envelope::new("commit").body(b);
    env.q = cm.q;
    env.c = cm.c;
    env.n = cm.n;
    env
}

fn bin(b: &Value) -> Option<&[u8]> {
    match b {
        Value::Binary(v) => Some(v),
        _ => None,
    }
}

impl Session {
    pub fn new(cfg: Config) -> Self {
        Session {
            id: rand_hex(8),
            cfg,
            clients: BTreeMap::new(),
            conns: HashMap::new(),
            next_client: 1,
            seq: 0,
            log: VecDeque::new(),
            keys: HashMap::new(),
            props: HashMap::new(),
            struct_rev: HashMap::new(),
            holds: BTreeMap::new(),
            locks: BTreeSet::new(),
            invites: BTreeMap::new(),
            queue: VecDeque::new(),
            validating: None,
            replica: None,
            saved_q: 0,
            saved_hash: String::new(),
            snap_wait: VecDeque::new(),
            snap_busy: None,
            last_presence: 0,
            last_ping: 0,
            ended: false,
            handover: Handover::None,
            carried: vec![],
            now: 0,
            stats: Stats::default(),
        }
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }
    pub fn ended(&self) -> bool {
        self.ended
    }
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Handoff (spec § 10; locks pass on, § 5a): the whole session state as one value, and a session made from it.

    /// Everything a new host needs to carry on: the log and key versions, locks, the people (with their resume tokens, so
    /// they rejoin the new host without a new code or approval), the last save. Holds and invites are not carried: holds
    /// are re-asserted on rejoin, and the new host makes its own codes.
    pub fn export(&self) -> Value {
        self.state(None)
    }

    /// The state for a handoff to client `to` (the `handover` message): `to` becomes the host and the old host an editor,
    /// and every person carries a rejoin ticket: a sealed connection's own (from its handshake); a connection without frame
    /// encryption (the old host's in-process window; test servers) gets a random one, sent in its own `moved`.
    fn handover_state(&mut self, to: u32) -> Value {
        let sealed: BTreeSet<u32> = self
            .clients
            .values()
            .filter(|c| c.conn.and_then(|k| self.conns.get(&k)).map(|x| x.sealed).unwrap_or(false))
            .map(|c| c.id)
            .collect();
        for c in self.clients.values_mut() {
            if c.id != to && c.role != Role::Replica && (c.ticket.is_empty() || !sealed.contains(&c.id)) {
                let mut t = vec![0u8; 32];
                getrandom::fill(&mut t).expect("os randomness");
                c.ticket = t;
            }
        }
        self.state(Some(to))
    }

    fn state(&self, to: Option<u32>) -> Value {
        let s = |x: &str| Value::from(x);
        // Edits queued but not yet decided are not in the log: their authors re-send them to the new host.
        let mut undecided: HashMap<u32, u64> = HashMap::new();
        for e in self.queue.iter().chain(self.validating.iter()) {
            let x = undecided.entry(e.c).or_insert(e.n);
            *x = (*x).min(e.n);
        }
        let clients: Vec<Value> = self
            .clients
            .values()
            .filter(|c| c.state != State::Waiting && !c.token.is_empty() && c.role != Role::Replica)
            .map(|c| {
                let role = match to {
                    Some(t) if t == c.id => Role::Host,
                    Some(_) if c.role == Role::Host => Role::Editor,
                    _ => c.role,
                };
                let last_n = undecided.get(&c.id).map(|n| c.last_n.min(n - 1)).unwrap_or(c.last_n);
                let mut pairs = vec![
                    ("c", c.id.into()),
                    ("name", s(&c.name)),
                    ("device", s(&c.device)),
                    ("role", s(role.name())),
                    ("prefix", s(&c.prefix)),
                    ("token", s(&c.token)),
                    ("last_n", last_n.into()),
                ];
                if !c.ticket.is_empty() {
                    pairs.push(("ticket", Value::Binary(c.ticket.clone())));
                }
                if c.of != 0 {
                    pairs.push(("of", c.of.into()));
                }
                map(pairs)
            })
            .collect();
        let log: Vec<Value> = self
            .log
            .iter()
            .map(|c| map(vec![("q", c.q.into()), ("c", c.c.into()), ("n", c.n.into()), ("label", s(&c.label)), ("ops", c.ops.clone()), ("touched", c.touched.clone())]))
            .collect();
        let keys: Vec<Value> = self
            .keys
            .iter()
            .map(|(k, v)| {
                Value::Array(vec![s(k), v.rev.into(), v.by.into(), v.deleted.into(), self.struct_rev.get(k).copied().unwrap_or(0).into()])
            })
            .collect();
        let props: Vec<Value> =
            self.props.iter().map(|(k, (q, by))| Value::Array(vec![s(k), (*q).into(), (*by).into()])).collect();
        let invites: Vec<Value> = self
            .invites
            .values()
            .filter_map(|k| match &k.kind {
                KeyKind::Code(code) if k.expires > self.now => Some(map(vec![
                    ("code", s(code)),
                    ("secret", Value::Binary(k.secret.clone())),
                    ("one_time", k.one_time.into()),
                    ("left", (k.expires - self.now).into()),
                ])),
                _ => None,
            })
            .collect();
        map(vec![
            ("format", s("cadcraft-live-state")),
            ("v", 1.into()),
            ("session", s(&self.id)),
            ("app", s(&self.cfg.app)),
            ("schema", self.cfg.schema.into()),
            ("seq", self.seq.into()),
            ("next_client", self.next_client.into()),
            ("saved", map(vec![("q", self.saved_q.into()), ("hash", s(&self.saved_hash))])),
            ("locks", strs(&self.locks.iter().cloned().collect::<Vec<_>>())),
            ("clients", Value::Array(clients)),
            ("log", Value::Array(log)),
            ("keys", Value::Array(keys)),
            ("props", Value::Array(props)),
            ("invites", Value::Array(invites)),
        ])
    }

    /// A session carrying on from `export()` (on the new host). Everyone starts as gone and comes back with `resume`.
    pub fn import(cfg: Config, st: &Value, now: Ms) -> Result<Session, String> {
        if get_str(st, "format") != Some("cadcraft-live-state") || get_u64(st, "v") != Some(1) {
            return Err("not a session state".into());
        }
        if get_str(st, "app").unwrap_or("") != cfg.app || get_u64(st, "schema") != Some(cfg.schema as u64) {
            return Err("another app or schema".into());
        }
        let mut s = Session::new(cfg);
        s.id = get_str(st, "session").unwrap_or("").chars().take(64).collect();
        s.seq = get_u64(st, "seq").unwrap_or(0);
        s.next_client = get_u64(st, "next_client").unwrap_or(1) as u32;
        if let Some(sv) = get(st, "saved") {
            s.saved_q = get_u64(sv, "q").unwrap_or(0);
            s.saved_hash = get_str(sv, "hash").unwrap_or("").to_string();
        }
        s.locks = get_arr(st, "locks").iter().filter_map(|v| v.as_str().map(String::from)).collect();
        for c in get_arr(st, "clients") {
            let id = get_u64(c, "c").ok_or("client id")? as u32;
            let role = get_str(c, "role").and_then(Role::parse).unwrap_or(Role::Editor);
            s.clients.insert(
                id,
                Client {
                    id,
                    name: get_str(c, "name").unwrap_or("").into(),
                    device: get_str(c, "device").unwrap_or("").into(),
                    via: "lan".into(),
                    kid: String::new(),
                    proof: Value::Nil,
                    assertion: String::new(),
                    picture: Value::Nil,
                    aud: String::new(),
                    ticket: match get(c, "ticket") {
                        Some(Value::Binary(t)) if t.len() == 32 => t.clone(),
                        _ => vec![],
                    },
                    role,
                    prefix: get_str(c, "prefix").unwrap_or("").into(),
                    token: get_str(c, "token").unwrap_or("").into(),
                    conn: None,
                    state: State::Gone,
                    last_seen: now,
                    gone_at: now,
                    last_n: get_u64(c, "last_n").unwrap_or(0),
                    presence: Value::Nil,
                    presence_dirty: false,
                    bucket: RATE_PER_SEC,
                    bucket_at: now,
                    backlog: VecDeque::new(),
                    warned: false,
                    hash: String::new(),
                    refusals: VecDeque::new(),
                    of: get_u64(c, "of").unwrap_or(0) as u32,
                },
            );
        }
        for e in get_arr(st, "log") {
            s.log.push_back(Commit {
                q: get_u64(e, "q").ok_or("log seq")?,
                c: get_u64(e, "c").unwrap_or(0) as u32,
                n: get_u64(e, "n").unwrap_or(0),
                label: get_str(e, "label").unwrap_or("").into(),
                ops: get(e, "ops").cloned().unwrap_or(Value::Nil),
                touched: get(e, "touched").cloned().unwrap_or(Value::Array(vec![])),
            });
        }
        for k in get_arr(st, "keys") {
            let a = k.as_array().ok_or("key")?;
            let name = a.first().and_then(|v| v.as_str()).ok_or("key name")?.to_string();
            let sr = a.get(4).and_then(|v| v.as_u64()).unwrap_or(0);
            if sr > 0 {
                s.struct_rev.insert(name.clone(), sr);
            }
            s.keys.insert(
                name,
                KeyState {
                    rev: a.get(1).and_then(|v| v.as_u64()).unwrap_or(0),
                    by: a.get(2).and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                    deleted: a.get(3).and_then(|v| v.as_bool()).unwrap_or(false),
                },
            );
        }
        for p in get_arr(st, "props") {
            let a = p.as_array().ok_or("prop")?;
            let k = a.first().and_then(|v| v.as_str()).ok_or("prop key")?.to_string();
            s.props.insert(k, (a.get(1).and_then(|v| v.as_u64()).unwrap_or(0), a.get(2).and_then(|v| v.as_u64()).unwrap_or(0) as u32));
        }
        // Everyone who carries a ticket comes back over the network with it (handshake context `rejoin:<session>`), no
        // code and no approval; in Studio the new host itself attaches in-process (resume token only).
        let context = format!("rejoin:{}", s.id);
        let tickets: Vec<(u32, Vec<u8>)> = s.clients.values().filter(|c| !c.ticket.is_empty()).map(|c| (c.id, c.ticket.clone())).collect();
        for (id, t) in tickets {
            s.invites.insert(
                ticket_kid(id),
                Key { kind: KeyKind::Ticket, secret: t, context: context.clone(), one_time: false, ttl: TICKET_TTL, expires: TICKET_TTL, wrong: 0 },
            );
        }
        // Short codes still running on the old host carry on here (the new host shows and advertises them, § 4.7); invite
        // links don't: their relay room was the old host's.
        for i in get_arr(st, "invites") {
            let (Some(code), Some(Value::Binary(secret))) = (get_str(i, "code"), get(i, "secret")) else { continue };
            let Some((tag, parsed)) = crypto::parse_code(code) else { continue };
            if &parsed != secret || s.invites.contains_key(&tag) {
                continue;
            }
            let left = get_u64(i, "left").unwrap_or(0).min(24 * 3_600_000);
            if left == 0 {
                continue;
            }
            let one_time = get(i, "one_time").and_then(|v| v.as_bool()).unwrap_or(false);
            s.invites.insert(
                tag.clone(),
                Key { kind: KeyKind::Code(code.to_string()), secret: parsed, context: format!("lan:{tag}"), one_time, ttl: left, expires: now + left, wrong: 0 },
            );
            s.carried.push(tag);
        }
        Ok(s)
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Connections

    /// A transport connection opened. `trusted` = in-process (the host's window, the replica); `via` = how it came
    /// (`local`, `lan`, `relay`), decided by the transport — a link's secret also works on the LAN (LV.26).
    pub fn open(&mut self, conn: ConnId, trusted: bool, via: &str, now: Ms) {
        let secure = trusted || self.cfg.plain;
        self.conns.insert(
            conn,
            Conn {
                client: None,
                trusted,
                secure,
                sealed: false,
                hs: None,
                via: via.into(),
                kid: String::new(),
                opened: now,
                aud: String::new(),
                ticket: vec![],
                of: 0,
            },
        );
    }

    /// The transport closed (or a frame failed to open: net.rs closes and calls this).
    pub fn closed(&mut self, conn: ConnId, now: Ms) -> Vec<Out> {
        let mut out = vec![];
        if let Some(cx) = self.conns.remove(&conn) {
            if let Some(hs) = cx.hs {
                // Quit after message 2 without proving the code: a wrong try (LIVE-CRYPTO.md, the guesser rule).
                self.wrong_try(&hs.kid, now, &mut out);
            }
            if let Some(c) = cx.client {
                if self.clients.get(&c).map(|x| x.conn) == Some(Some(conn)) {
                    self.drop_client(c, now, false, &mut out);
                }
            }
        }
        out
    }

    fn drop_client(&mut self, c: u32, now: Ms, removed: bool, out: &mut Vec<Out>) {
        let Some(cl) = self.clients.get_mut(&c) else { return };
        if cl.state == State::Gone {
            return;
        }
        let was_waiting = cl.state == State::Waiting;
        let role = cl.role;
        cl.state = State::Gone;
        cl.gone_at = now;
        if self.handover == Handover::Waiting(c) {
            // The new host went before it said where it is: hand over called off, edits commit here again.
            self.handover = Handover::None;
            let b = map(vec![("to", c.into()), ("reason", "gone".into())]);
            self.to_role(Role::Host, Envelope::new("handover-off").body(b), out);
        }
        let Some(cl) = self.clients.get_mut(&c) else { return };
        if removed {
            cl.token.clear(); // no rejoin
            self.invites.remove(&ticket_kid(c));
        }
        if cl.token.is_empty() {
            // Removed, or left with `bye`: their assistants go with them (LV.18), and keys not used yet.
            self.remove_assistants(c, now, out);
        }
        let Some(cl) = self.clients.get_mut(&c) else { return };
        if let Some(conn) = cl.conn.take() {
            self.conns.remove(&conn);
            out.push(Out::Close(conn, if removed { "removed".into() } else { "gone".into() }));
        }
        if was_waiting {
            self.clients.remove(&c);
            return;
        }
        if self.replica == Some(c) {
            self.replica = None;
            if let Some(e) = self.validating.take() {
                self.queue.push_front(e); // decided again without a validator
            }
        }
        if self.snap_busy == Some(c) {
            self.snap_busy = None;
        }
        self.snap_wait.retain(|x| *x != c);
        let released: Vec<String> = self.holds.iter().filter(|(_, (h, _))| *h == c).map(|(k, _)| k.clone()).collect();
        for k in &released {
            self.holds.remove(k);
        }
        if !released.is_empty() {
            self.broadcast(&holds_change(&[], &released), None, out);
        }
        if role != Role::Replica {
            let b = map(vec![("c", c.into()), ("state", if removed { "removed" } else { "gone" }.into())]);
            self.broadcast(&Envelope::new("peer").body(b), Some(c), out);
        }
        if role == Role::Host && !removed {
            // The host quit: every client keeps its copy (§ 7).
            self.end("host-left", out);
        }
        self.pump_snapshots(out);
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Frames

    pub fn frame(&mut self, conn: ConnId, env: Envelope, now: Ms) -> Vec<Out> {
        self.now = now;
        let mut out = vec![];
        if self.ended {
            out.push(Out::Close(conn, "ended".into()));
            return out;
        }
        let Some(cx) = self.conns.get(&conn) else { return out };
        if !cx.secure {
            // A test harness's host or replica (local token, never set in Studio) says hello without a handshake.
            let tok = !self.cfg.local_token.is_empty() && get_str(&env.b, "token") == Some(self.cfg.local_token.as_str());
            if env.t == "hello" && tok && cx.client.is_none() {
                self.conns.get_mut(&conn).unwrap().secure = true;
                self.on_hello(conn, env, now, &mut out);
                self.pump(&mut out);
                return out;
            }
            if env.t == "pake" {
                self.on_pake(conn, &env, now, &mut out);
            } else {
                self.conns.remove(&conn);
                out.push(Out::Close(conn, "handshake expected".into()));
            }
            return out;
        }
        match cx.client {
            None => self.on_hello(conn, env, now, &mut out),
            Some(c) => {
                if let Some(cl) = self.clients.get_mut(&c) {
                    cl.last_seen = now;
                }
                self.on_message(c, conn, env, now, &mut out);
            }
        }
        self.pump(&mut out);
        out
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Handshake (docs/LIVE-CRYPTO.md): message 1 {kid, m} → 2 {m} → 3 {m} → 4 {m}, then sealed frames.

    fn on_pake(&mut self, conn: ConnId, env: &Envelope, now: Ms, out: &mut Vec<Out>) {
        let m = get(&env.b, "m").and_then(bin).map(|b| b.to_vec()).unwrap_or_default();
        let cx = self.conns.get_mut(&conn).unwrap();
        match cx.hs.as_mut() {
            None => {
                let kid = get_str(&env.b, "kid").unwrap_or("").to_string();
                let Some(key) = self.invites.get(&kid).filter(|k| k.expires > now) else {
                    // Unknown or expired secret: nothing was tried, so not counted.
                    self.conns.remove(&conn);
                    out.push(Out::Close(conn, "unknown code".into()));
                    return;
                };
                let mut host = crypto::HostHandshake::new(&key.secret, &key.context);
                match host.receive(&m) {
                    Ok(reply) => {
                        cx.kid = kid.clone();
                        cx.hs = Some(Handshake { kid, started: now, host: Some(host) });
                        out.push(Out::To(conn, Envelope::new("pake").body(map(vec![("m", Value::Binary(reply))]))));
                    }
                    Err(_) => {
                        self.conns.remove(&conn);
                        out.push(Out::Close(conn, "bad handshake".into()));
                    }
                }
            }
            Some(hs) => {
                let kid = hs.kid.clone();
                let Some(host) = hs.host.take() else { return };
                match host.finish(&m) {
                    Ok((confirm, session)) => {
                        cx.hs = None;
                        cx.secure = true;
                        cx.sealed = true;
                        // Bound to this connection, never sent: the joiner's signed-in name is checked against `aud`
                        // (LV.19), and `ticket` brings it back after a host handoff (LV.17).
                        cx.aud = b64url(&session.exporter("assertion"));
                        cx.ticket = session.exporter("ticket").to_vec();
                        out.push(Out::To(conn, Envelope::new("pake").body(map(vec![("m", Value::Binary(confirm))]))));
                        out.push(Out::Secure(conn, Box::new(session)));
                        let mut of = 0;
                        if let Some(key) = self.invites.get_mut(&kid) {
                            key.wrong = 0;
                            if let KeyKind::Assistant(p) = &key.kind {
                                of = *p;
                            }
                            if key.one_time {
                                self.invites.remove(&kid);
                                if of == 0 {
                                    self.to_role(Role::Host, code_gone(&kid, "used"), out);
                                }
                            }
                        }
                        if of != 0 {
                            if let Some(cx) = self.conns.get_mut(&conn) {
                                cx.of = of; // an assistant's key: hello joins it as that person's assistant (LV.18)
                            }
                        }
                    }
                    Err(_) => {
                        // wrong_code (or a malformed confirmation): a wrong try, and the connection closes.
                        cx.hs = None;
                        self.conns.remove(&conn);
                        out.push(Out::Close(conn, "wrong code".into()));
                        self.wrong_try(&kid, now, out);
                    }
                }
            }
        }
    }

    /// Counts a wrong try against a secret; three in a row replace it (spec § 6).
    fn wrong_try(&mut self, kid: &str, now: Ms, out: &mut Vec<Out>) {
        self.stats.wrong_tries += 1;
        let Some(key) = self.invites.get_mut(kid) else { return };
        key.wrong += 1;
        if key.wrong < WRONG_TRIES {
            return;
        }
        self.stats.rotations += 1;
        let key = self.invites.remove(kid).unwrap();
        if matches!(key.kind, KeyKind::Ticket | KeyKind::Assistant(_)) {
            return; // a ticket or an assistant's key is nobody's invite: it is just gone
        }
        self.to_role(Role::Host, code_gone(kid, "wrong-tries"), out);
        if let KeyKind::Code(_) = key.kind {
            self.make_code(key.one_time, key.ttl, Some(kid), now, out);
        }
    }

    /// A new short code (`invite`), told to the host.
    fn make_code(&mut self, one_time: bool, ttl: Ms, replaces: Option<&str>, now: Ms, out: &mut Vec<Out>) {
        let (code, tag, secret) = loop {
            let code = crypto::new_code();
            let (tag, secret) = crypto::parse_code(&code).expect("own code parses");
            if !self.invites.contains_key(&tag) {
                break (code, tag, secret);
            }
        };
        let expires = now + ttl;
        self.invites.insert(
            tag.clone(),
            Key {
                kind: KeyKind::Code(code.clone()),
                secret,
                context: format!("lan:{tag}"),
                one_time,
                ttl,
                expires,
                wrong: 0,
            },
        );
        let mut pairs = vec![
            ("code", Value::from(code)),
            ("tag", Value::from(tag)),
            ("expires", Value::from(ttl)),
            ("one_time", Value::from(one_time)),
        ];
        if let Some(old) = replaces {
            pairs.push(("reason", "wrong-tries".into()));
            pairs.push(("replaces", old.into()));
        }
        self.to_role(Role::Host, Envelope::new("code").body(map(pairs)), out);
    }

    /// Something for the host's window from outside the session (the relay's state, relay.rs).
    pub fn notice(&mut self, env: Envelope) -> Vec<Out> {
        let mut out = vec![];
        self.to_role(Role::Host, env, &mut out);
        out
    }

    // ---------------------------------------------------------------------------------------------------------------
    // hello

    fn on_hello(&mut self, conn: ConnId, env: Envelope, now: Ms, out: &mut Vec<Out>) {
        let deny = |s: &mut Session, out: &mut Vec<Out>, reason: &str| {
            s.conns.remove(&conn);
            out.push(Out::To(conn, Envelope::new("deny").body(map(vec![("reason", reason.into())]))));
            out.push(Out::Close(conn, reason.into()));
        };
        if env.t != "hello" {
            self.conns.remove(&conn);
            out.push(Out::Close(conn, "hello expected".into()));
            return;
        }
        let b = &env.b;
        if get_u64(b, "v").unwrap_or(0) != PROTOCOL as u64 {
            return deny(self, out, "protocol");
        }
        let app = get_str(b, "app").unwrap_or("");
        if !self.cfg.app.is_empty() && app != self.cfg.app {
            return deny(self, out, "app");
        }
        if get_u64(b, "schema").unwrap_or(0) != self.cfg.schema as u64 {
            return deny(self, out, "schema");
        }
        // A privileged connection claims host / replica: in-process, or a test server's local token.
        let want = get_str(b, "role").and_then(Role::parse);
        if matches!(want, Some(Role::Host) | Some(Role::Replica)) {
            let trusted = self.conns[&conn].trusted;
            let tok_ok = !self.cfg.local_token.is_empty() && get_str(b, "token") == Some(self.cfg.local_token.as_str());
            if !(trusted || tok_ok) {
                return deny(self, out, "role");
            }
            let role = want.unwrap();
            if role == Role::Host && self.clients.values().any(|c| c.role == Role::Host && c.state != State::Gone) {
                return deny(self, out, "host-taken");
            }
            let c = self.new_client(conn, b, role, now);
            if role == Role::Replica {
                self.replica = Some(c);
            }
            self.activate(c, out);
            return;
        }
        // Rejoin: client id + resume token, no new approval (§ 7).
        if let Some(r) = get(b, "resume") {
            let rc = get_u64(r, "c").unwrap_or(0) as u32;
            let tok = get_str(r, "token").unwrap_or("");
            let ok = self.clients.get(&rc).map(|cl| !cl.token.is_empty() && cl.token == tok && cl.state != State::Waiting);
            if ok != Some(true) {
                return deny(self, out, "resume");
            }
            if let Some(old) = self.clients.get(&rc).and_then(|cl| cl.conn) {
                self.conns.remove(&old);
                out.push(Out::Close(old, "replaced".into()));
            }
            let from = get_u64(r, "q").unwrap_or(0);
            let (via, kid) = {
                let cx = &self.conns[&conn];
                (cx.via.clone(), cx.kid.clone())
            };
            let cl = self.clients.get_mut(&rc).unwrap();
            let was_gone = cl.state == State::Gone;
            cl.conn = Some(conn);
            cl.state = State::Active;
            cl.last_seen = now;
            cl.via = via;
            if !kid.is_empty() && !kid.starts_with("h:") {
                cl.kid = kid;
            }
            let (aud, ticket) = {
                let cx = &self.conns[&conn];
                (cx.aud.clone(), cx.ticket.clone())
            };
            let cl = self.clients.get_mut(&rc).unwrap();
            cl.aud = aud;
            if cl.ticket.is_empty() && !ticket.is_empty() {
                cl.ticket = ticket; // the first sealed connection's ticket stays the person's key for the session
            }

            cl.hash = get_str(b, "hash").unwrap_or("").to_string();
            self.conns.get_mut(&conn).unwrap().client = Some(rc);
            // Holds come back if nobody took the objects.
            let mut taken = vec![];
            for k in get_arr(r, "holds").iter().filter_map(|v| v.as_str()).take(10_000) {
                if !self.holds.contains_key(k) {
                    self.holds.insert(k.to_string(), (rc, now + HOLD_TTL));
                    taken.push((k.to_string(), rc));
                }
            }
            self.welcome(rc, Some(from), out);
            self.rejoin_key(rc);
            if self.clients[&rc].role == Role::Host {
                self.tell_carried(out);
            }
            // Refusals it may have missed: every one at or after the oldest edit it still has pending.
            let oldest = get_u64(r, "n").unwrap_or(u64::MAX);
            let again: Vec<Envelope> = self.clients[&rc]
                .refusals
                .iter()
                .filter(|(n, _)| *n >= oldest)
                .map(|(n, b)| {
                    let mut e = Envelope::new("refuse").body(b.clone());
                    e.n = *n;
                    e
                })
                .collect();
            for e in again {
                out.push(Out::To(conn, e));
            }
            if !taken.is_empty() {
                self.broadcast(&holds_change(&taken, &[]), None, out);
            }
            if was_gone {
                let pb = map(vec![("c", rc.into()), ("state", "back".into())]);
                self.broadcast(&Envelope::new("peer").body(pb), Some(rc), out);
            }
            return;
        }
        // An assistant (LV.18): its person's one-time key, through the handshake (`Conn::of`) or — on an in-process or plain
        // connection, which has no handshake — in `hello.assistant {kid, secret}`.
        let mut of = self.conns[&conn].of;
        if of == 0 {
            if let Some(a) = get(b, "assistant") {
                // No handshake to carry the key: in-process (Studio), a plain test server, or a test harness's local token.
                let tok_ok = !self.cfg.local_token.is_empty() && get_str(b, "token") == Some(self.cfg.local_token.as_str());
                let open = self.conns[&conn].trusted || self.cfg.plain || tok_ok;
                let kid = get_str(a, "kid").unwrap_or("").to_string();
                let sec = get(a, "secret").and_then(bin).map(|x| x.to_vec()).unwrap_or_default();
                let ok = open
                    && matches!(self.invites.get(&kid), Some(k) if matches!(k.kind, KeyKind::Assistant(_)) && k.expires > now && same_bytes(&k.secret, &sec));
                if !ok {
                    return deny(self, out, "role");
                }
                if let Some(Key { kind: KeyKind::Assistant(p), .. }) = self.invites.remove(&kid) {
                    of = p;
                }
            }
        }
        if of != 0 {
            // The person must still be in, and able to edit: a viewer's (or a gone person's) assistant is refused.
            let person_ok = self.clients.get(&of).map(|p| p.state == State::Active && matches!(p.role, Role::Host | Role::Editor)).unwrap_or(false);
            if !person_ok {
                return deny(self, out, "role");
            }
        }
        let people = self.clients.values().filter(|c| c.state != State::Gone && c.role != Role::Replica).count();
        // max_clients guests besides the host.
        if people > self.cfg.max_clients {
            return deny(self, out, "full");
        }
        let c = self.new_client(conn, b, if of != 0 { Role::Assistant } else { Role::Editor }, now);
        self.clients.get_mut(&c).unwrap().of = of;
        // The host's own assistant needs no card: the host turned assistants on in their own Settings.
        if self.cfg.auto_approve || (of != 0 && self.clients[&of].role == Role::Host) {
            self.activate(c, out);
            return;
        }
        out.push(Out::To(conn, Envelope::new("waiting")));
        let cl = &self.clients[&c];
        let mut jb = vec![
            ("c", c.into()),
            ("name", cl.name.clone().into()),
            ("device", cl.device.clone().into()),
            ("via", cl.via.clone().into()),
            ("kid", cl.kid.clone().into()),
            ("proof", cl.proof.clone()),
            ("assertion", cl.assertion.clone().into()),
            ("picture", cl.picture.clone()),
        ];
        // The host's window checks `assertion` against this connection's audience (LV.19); only after a handshake.
        if !cl.aud.is_empty() {
            jb.push(("aud", cl.aud.clone().into()));
        }
        // An assistant asking in (LV.18): whose it is, for the approve card ("Ana's assistant").
        if cl.of != 0 {
            let pn = self.clients.get(&cl.of).map(|p| p.name.clone()).unwrap_or_default();
            jb.push(("assistant", map(vec![("of", cl.of.into()), ("of_name", pn.into())])));
        }
        let jb = map(jb);
        self.to_role(Role::Host, Envelope::new("join").body(jb), out);
    }

    fn new_client(&mut self, conn: ConnId, b: &Value, role: Role, now: Ms) -> u32 {
        let id = self.next_client;
        self.next_client += 1;
        let cx = self.conns.get_mut(&conn).unwrap();
        cx.client = Some(id);
        let cl = Client {
            id,
            name: get_str(b, "name").unwrap_or("").chars().take(80).collect(),
            device: get_str(b, "device").unwrap_or("").chars().take(80).collect(),
            via: cx.via.clone(),
            kid: cx.kid.clone(),
            proof: get(b, "proof").cloned().unwrap_or(Value::Nil),
            assertion: get_str(b, "assertion").filter(|a| a.len() <= 2048).unwrap_or("").to_string(),
            picture: match get(b, "picture") {
                Some(Value::Binary(p)) if p.len() <= 16 * 1024 => Value::Binary(p.clone()),
                _ => Value::Nil,
            },
            aud: cx.aud.clone(),
            ticket: cx.ticket.clone(),
            role,
            prefix: prefix_for(id),
            token: rand_hex(16),
            conn: Some(conn),
            state: State::Waiting,
            last_seen: now,
            gone_at: 0,
            last_n: 0,
            presence: Value::Nil,
            presence_dirty: false,
            bucket: RATE_PER_SEC,
            bucket_at: now,
            backlog: VecDeque::new(),
            warned: false,
            hash: get_str(b, "hash").unwrap_or("").to_string(),
            refusals: VecDeque::new(),
            of: 0,
        };
        self.clients.insert(id, cl);
        id
    }

    /// A person in over a sealed connection gets a rejoin key: their connection's `exporter("ticket")` under `h:<c>`, so a
    /// drop after their code or link expired still finds the way back (and a handoff carries it to the new host).
    fn rejoin_key(&mut self, c: u32) {
        let Some(cl) = self.clients.get(&c) else { return };
        let sealed = cl.conn.and_then(|k| self.conns.get(&k)).map(|x| x.sealed).unwrap_or(false);
        let kid = ticket_kid(c);
        if !sealed || cl.ticket.is_empty() || self.invites.contains_key(&kid) {
            return;
        }
        let key = Key {
            kind: KeyKind::Ticket,
            secret: cl.ticket.clone(),
            context: format!("rejoin:{}", self.id),
            one_time: false,
            ttl: TICKET_TTL,
            expires: TICKET_TTL,
            wrong: 0,
        };
        self.invites.insert(kid, key);
    }

    /// The new host is in after a handoff: the codes that came with the session, as `code {…, carried: true}`.
    fn tell_carried(&mut self, out: &mut Vec<Out>) {
        for tag in std::mem::take(&mut self.carried) {
            let Some(k) = self.invites.get(&tag) else { continue };
            let KeyKind::Code(code) = &k.kind else { continue };
            let b = map(vec![
                ("code", code.as_str().into()),
                ("tag", tag.as_str().into()),
                ("expires", k.expires.saturating_sub(self.now).into()),
                ("one_time", k.one_time.into()),
                ("carried", true.into()),
            ]);
            self.to_role(Role::Host, Envelope::new("code").body(b), out);
        }
    }

    fn activate(&mut self, c: u32, out: &mut Vec<Out>) {
        let Some(cl) = self.clients.get_mut(&c) else { return };
        cl.state = State::Active;
        let role = cl.role;
        self.welcome(c, None, out);
        self.rejoin_key(c);
        if role == Role::Host {
            self.tell_carried(out);
        }
        if role != Role::Replica {
            let cl = &self.clients[&c];
            let mut pb = vec![
                ("c", c.into()),
                ("state", "in".into()),
                ("name", cl.name.clone().into()),
                ("role", role.name().into()),
                ("via", cl.via.clone().into()),
            ];
            if cl.of != 0 {
                pb.push(("of", cl.of.into()));
            }
            self.broadcast(&Envelope::new("peer").body(map(pb)), Some(c), out);
        }
    }

    /// `welcome` + catch-up: the commits since `from` (rejoin) or since the last save (same file), else a snapshot.
    fn welcome(&mut self, c: u32, from: Option<u64>, out: &mut Vec<Out>) {
        let cl = &self.clients[&c];
        let conn = cl.conn.unwrap();
        // The log reaches back to (first kept seq - 1): a client at that seq or later can be caught up from it.
        let reach = self.log.front().map(|x| x.q - 1).unwrap_or(self.seq);
        let from = match from {
            Some(q) if q <= self.seq && q >= reach => Some(q),
            Some(_) => None,
            None if !cl.hash.is_empty() && cl.hash == self.saved_hash && self.saved_q >= reach => Some(self.saved_q),
            None => None,
        };
        let peers: Vec<Value> = self
            .clients
            .values()
            .filter(|p| p.id != c && p.state == State::Active && p.role != Role::Replica)
            .map(|p| {
                let mut v = vec![
                    ("c", p.id.into()),
                    ("name", p.name.clone().into()),
                    ("role", p.role.name().into()),
                    ("via", p.via.clone().into()),
                ];
                if p.of != 0 {
                    v.push(("of", p.of.into()));
                }
                map(v)
            })
            .collect();
        let holds: Vec<(Value, Value)> =
            self.holds.iter().map(|(k, (h, _))| (Value::from(k.as_str()), Value::from(*h))).collect();
        let locks: Vec<Value> = self.locks.iter().map(|k| Value::from(k.as_str())).collect();
        // An assistant shares its person's copy (LV.18): it needs no catch-up of its own, only the session's seq.
        let assistant = cl.of != 0;
        let catchup = match (cl.role, from) {
            (Role::Replica, _) => "none",
            (_, Some(_)) => "log",
            (_, None) if assistant => "none",
            (_, None) => "snapshot",
        };
        let b = map(vec![
            ("c", c.into()),
            ("prefix", cl.prefix.clone().into()),
            ("role", cl.role.name().into()),
            ("token", cl.token.clone().into()),
            ("session", self.id.clone().into()),
            ("app", self.cfg.app.clone().into()),
            ("schema", self.cfg.schema.into()),
            ("peers", Value::Array(peers)),
            ("holds", Value::Map(holds)),
            ("locks", Value::Array(locks)),
            ("saved", map(vec![("q", self.saved_q.into()), ("hash", self.saved_hash.clone().into())])),
            ("catchup", catchup.into()),
            ("last_n", cl.last_n.into()),
        ]);
        let mut env = Envelope::new("welcome").body(b);
        env.s = self.id.clone();
        env.q = self.seq;
        out.push(Out::To(conn, env));
        match catchup {
            "log" => {
                let from = from.unwrap();
                for cm in self.log.iter().filter(|x| x.q > from) {
                    out.push(Out::To(conn, commit_env(cm)));
                }
            }
            "snapshot" => {
                self.snap_wait.push_back(c);
                self.pump_snapshots(out);
            }
            _ => {}
        }
    }

    /// One snapshot at a time (§ 7): ask the replica, or the host window when there is no replica.
    fn pump_snapshots(&mut self, out: &mut Vec<Out>) {
        if self.snap_busy.is_some() {
            return;
        }
        while let Some(c) = self.snap_wait.pop_front() {
            if self.clients.get(&c).map(|x| x.state == State::Active) != Some(true) {
                continue;
            }
            let src = self.replica.or_else(|| self.role_client(Role::Host)).filter(|s| *s != c);
            let Some(src) = src else {
                // Nobody can make one (a test server without host or replica): an empty snapshot at seq 0, then the log.
                let mut e = Envelope::new("snap").body(map(vec![("i", 0.into()), ("of", 0.into())]));
                e.q = 0;
                self.send(c, e, out);
                let reach_ok = self.log.front().map(|x| x.q <= 1).unwrap_or(true);
                if reach_ok {
                    for cm in self.log.iter() {
                        self.send(c, commit_env(cm), out);
                    }
                }
                continue;
            };
            self.snap_busy = Some(c);
            self.send(src, Envelope::new("snapreq").body(map(vec![("for", c.into())])), out);
            return;
        }
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Messages from a member

    fn on_message(&mut self, c: u32, conn: ConnId, env: Envelope, now: Ms, out: &mut Vec<Out>) {
        let (role, state) = {
            let cl = &self.clients[&c];
            (cl.role, cl.state)
        };
        if state == State::Waiting {
            // Nothing reaches a joiner before approval, and nothing from it counts except bye.
            if env.t == "bye" {
                self.conns.remove(&conn);
                self.clients.remove(&c);
                out.push(Out::Close(conn, "bye".into()));
            }
            return;
        }
        match env.t.as_str() {
            "ping" => out.push(Out::To(conn, Envelope { t: "pong".into(), n: env.n, ..Default::default() })),
            "pong" => {}
            "bye" => {
                // A clean leave: no rejoin expected.
                if let Some(cl) = self.clients.get_mut(&c) {
                    cl.token.clear();
                }
                self.drop_client(c, now, false, out);
            }
            "edit" => self.on_edit(c, conn, env, now, out),
            "assistant" => self.on_assistant(c, conn, now, out),
            "hold" => {
                let keys: Vec<String> = get_arr(&env.b, "keys").iter().filter_map(|v| v.as_str().map(String::from)).collect();
                let mut set = vec![];
                for k in keys.into_iter().take(10_000) {
                    match self.holds.get(&k) {
                        Some((h, _)) if *h != c => {}
                        prev => {
                            let fresh = prev.is_none();
                            self.holds.insert(k.clone(), (c, now + HOLD_TTL));
                            if fresh {
                                set.push((k, c));
                            }
                        }
                    }
                }
                if !set.is_empty() {
                    self.broadcast(&holds_change(&set, &[]), None, out);
                }
            }
            "release" => {
                let all = get(&env.b, "all").and_then(|v| v.as_bool()) == Some(true);
                let keys: Vec<String> = if all {
                    self.holds.iter().filter(|(_, (h, _))| *h == c).map(|(k, _)| k.clone()).collect()
                } else {
                    get_arr(&env.b, "keys").iter().filter_map(|v| v.as_str().map(String::from)).collect()
                };
                let mut gone = vec![];
                for k in keys {
                    if self.holds.get(&k).map(|(h, _)| *h) == Some(c) {
                        self.holds.remove(&k);
                        gone.push(k);
                    }
                }
                if !gone.is_empty() {
                    self.broadcast(&holds_change(&[], &gone), None, out);
                }
            }
            "presence" => {
                let cl = self.clients.get_mut(&c).unwrap();
                cl.presence = env.b;
                cl.presence_dirty = true;
            }
            // ---- host only
            "approve" | "deny" | "remove" | "role" if role == Role::Host => self.host_people(&env, now, out),
            "lock" | "unlock" if role == Role::Host => {
                let keys: Vec<String> = get_arr(&env.b, "keys").iter().filter_map(|v| v.as_str().map(String::from)).collect();
                let (mut add, mut del) = (vec![], vec![]);
                for k in keys.into_iter().take(100_000) {
                    if env.t == "lock" {
                        if self.locks.insert(k.clone()) {
                            add.push(k);
                        }
                    } else if self.locks.remove(&k) {
                        del.push(k);
                    }
                }
                if !add.is_empty() || !del.is_empty() {
                    let b = map(vec![("add", strs(&add)), ("del", strs(&del))]);
                    self.broadcast(&Envelope::new("locks").body(b), None, out);
                }
            }
            "invite" if role == Role::Host => {
                let one_time = get(&env.b, "one_time").and_then(|v| v.as_bool()).unwrap_or(false);
                let ttl = get_u64(&env.b, "ttl").unwrap_or(CODE_TTL).clamp(10_000, 24 * 3_600_000);
                self.make_code(one_time, ttl, None, now, out);
            }
            "key" if role == Role::Host => {
                let kid = get_str(&env.b, "kid").unwrap_or("").to_string();
                let secret = get(&env.b, "secret").and_then(bin).map(|b| b.to_vec()).unwrap_or_default();
                let context = get_str(&env.b, "context").unwrap_or("").to_string();
                if kid.is_empty() || kid.len() > 64 || secret.len() < 16 || !context.starts_with("relay:") {
                    return;
                }
                let ttl = get_u64(&env.b, "ttl").unwrap_or(8 * 3_600_000);
                let one_time = get(&env.b, "one_time").and_then(|v| v.as_bool()).unwrap_or(false);
                self.invites.insert(
                    kid,
                    Key { kind: KeyKind::Link, secret, context, one_time, ttl, expires: now + ttl, wrong: 0 },
                );
            }
            "uninvite" if role == Role::Host => {
                let kids: Vec<String> = match get_str(&env.b, "kid") {
                    Some(k) => vec![k.to_string()],
                    // "Stop inviting" ends codes and links; handoff tickets are not invites.
                    None => self.invites.iter().filter(|(_, k)| k.kind != KeyKind::Ticket).map(|(k, _)| k.clone()).collect(),
                };
                for k in kids {
                    if self.invites.remove(&k).is_some() {
                        self.to_role(Role::Host, code_gone(&k, "stopped"), out);
                    }
                }
            }
            "save" if role == Role::Host => {
                self.saved_q = get_u64(&env.b, "q").unwrap_or(self.seq).min(self.seq);
                self.saved_hash = get_str(&env.b, "hash").unwrap_or("").chars().take(200).collect();
                let b = map(vec![
                    ("q", self.saved_q.into()),
                    ("hash", self.saved_hash.clone().into()),
                    ("at", get(&env.b, "at").cloned().unwrap_or(Value::Nil)),
                ]);
                self.broadcast(&Envelope::new("saved").body(b), None, out);
            }
            "end" if role == Role::Host => self.end("host-ended", out),
            "handover" if role == Role::Host => self.on_handover(c, &env, out),
            "moved" => self.on_moved(c, &env, out),
            // ---- replica (or the host window answering a snapshot request)
            "valid" if role == Role::Replica => self.on_valid(&env, out),
            "snap" if role == Role::Replica || role == Role::Host => self.on_snap(env, out),
            _ => {}
        }
    }

    /// `assistant` (LV.18): a person (host or editor) asks for a one-time key for the assistant connected to their window,
    /// which then joins as its own client (role `assistant`, its own undo; the host approves it, the host's own at once).
    fn on_assistant(&mut self, c: u32, conn: ConnId, now: Ms, out: &mut Vec<Out>) {
        let reply = |b: Value| Envelope::new("assistant-key").body(b);
        let cl = &self.clients[&c];
        if !matches!(cl.role, Role::Host | Role::Editor) || cl.of != 0 {
            out.push(Out::To(conn, reply(map(vec![("reason", "role".into())]))));
            return;
        }
        let used = self.clients.values().filter(|x| x.of == c && x.state != State::Gone).count()
            + self.invites.values().filter(|k| k.kind == KeyKind::Assistant(c) && k.expires > now).count();
        if used >= MAX_ASSISTANTS {
            out.push(Out::To(conn, reply(map(vec![("reason", "limit".into())]))));
            return;
        }
        let kid = format!("a:{c}:{}", rand_hex(4));
        let mut secret = vec![0u8; 32];
        getrandom::fill(&mut secret).expect("os randomness");
        let context = format!("assistant:{}", self.id);
        self.invites.insert(
            kid.clone(),
            Key { kind: KeyKind::Assistant(c), secret: secret.clone(), context: context.clone(), one_time: true, ttl: ASSISTANT_KEY_TTL, expires: now + ASSISTANT_KEY_TTL, wrong: 0 },
        );
        out.push(Out::To(conn, reply(map(vec![("kid", kid.into()), ("secret", Value::Binary(secret)), ("context", context.into())]))));
    }

    /// A person's assistants leave with them (removed, left, or made a viewer): `removed {reason: person}`; unused keys go.
    fn remove_assistants(&mut self, person: u32, now: Ms, out: &mut Vec<Out>) {
        self.invites.retain(|_, k| k.kind != KeyKind::Assistant(person));
        let theirs: Vec<u32> = self.clients.values().filter(|x| x.of == person && x.state != State::Gone).map(|x| x.id).collect();
        for a in &theirs {
            if let Some(conn) = self.clients.get(a).and_then(|x| x.conn) {
                out.push(Out::To(conn, Envelope::new("removed").body(map(vec![("reason", "person".into())]))));
            }
            self.drop_client(*a, now, true, out);
        }
        if !theirs.is_empty() {
            self.rotate_keys(out);
        }
    }

    fn host_people(&mut self, env: &Envelope, now: Ms, out: &mut Vec<Out>) {
        let target = get_u64(&env.b, "c").unwrap_or(0) as u32;
        let Some(cl) = self.clients.get(&target) else { return };
        if cl.role == Role::Host || cl.role == Role::Replica {
            return;
        }
        let state = cl.state;
        match env.t.as_str() {
            "approve" if state == State::Waiting => {
                let role = get_str(&env.b, "role").and_then(Role::parse).unwrap_or(Role::Editor);
                let role = if matches!(role, Role::Host | Role::Replica) { Role::Editor } else { role };
                // An assistant is let in as an assistant (or to look only), never as a person.
                let role = if cl.of != 0 && role != Role::Viewer { Role::Assistant } else { role };
                self.clients.get_mut(&target).unwrap().role = role;
                self.activate(target, out);
            }
            "deny" if state == State::Waiting => {
                if let Some(conn) = cl.conn {
                    out.push(Out::To(conn, Envelope::new("deny").body(map(vec![("reason", "host".into())]))));
                    out.push(Out::Close(conn, "denied".into()));
                    self.conns.remove(&conn);
                }
                self.clients.remove(&target);
            }
            "remove" if state != State::Gone => {
                if let Some(conn) = cl.conn {
                    out.push(Out::To(conn, Envelope::new("removed").body(map(vec![("reason", "host".into())]))));
                }
                self.drop_client(target, now, true, out);
                self.rotate_keys(out);
            }
            "role" if state == State::Active => {
                let role = get_str(&env.b, "role").and_then(Role::parse).unwrap_or(Role::Viewer);
                let role = if cl.of != 0 && role == Role::Editor { Role::Assistant } else { role };
                if matches!(role, Role::Editor | Role::Viewer | Role::Assistant) {
                    self.clients.get_mut(&target).unwrap().role = role;
                    let b = map(vec![("c", target.into()), ("state", "role".into()), ("role", role.name().into())]);
                    self.broadcast(&Envelope::new("peer").body(b), None, out);
                    // A person who can only look now: their assistants can't stay to edit (LV.18).
                    if role == Role::Viewer && self.clients[&target].of == 0 {
                        self.remove_assistants(target, now, out);
                    }
                }
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Host handoff (spec § 10): handover → the state to the new host → its `moved` → everyone follows with a ticket.

    /// `handover {to}` from the host: nothing more commits here (queued edits stay undecided and are re-sent to the new
    /// host), and the new host gets the whole state.
    fn on_handover(&mut self, host: u32, env: &Envelope, out: &mut Vec<Out>) {
        if self.handover != Handover::None {
            return;
        }
        let to = get_u64(&env.b, "to").unwrap_or(0) as u32;
        let ok = self
            .clients
            .get(&to)
            .map(|c| c.id != host && c.state == State::Active && matches!(c.role, Role::Editor | Role::Viewer))
            == Some(true);
        if !ok {
            let b = map(vec![("to", to.into()), ("reason", "unknown".into())]);
            self.send(host, Envelope::new("handover-off").body(b), out);
            return;
        }
        self.handover = Handover::Waiting(to);
        let st = self.handover_state(to);
        let mut bytes = vec![];
        rmpv::encode::write_value(&mut bytes, &st).expect("state encodes");
        // Did anyone come through the relay (or is a link open)? Then the new host should open a room of its own (§ 4.7).
        let relay = self.invites.values().any(|k| k.kind == KeyKind::Link)
            || self.clients.values().any(|c| c.state == State::Active && c.via == "relay");
        let chunks: Vec<&[u8]> = bytes.chunks(HANDOVER_CHUNK).collect();
        let of = chunks.len();
        for (i, part) in chunks.into_iter().enumerate() {
            let mut b = vec![("from", host.into()), ("i", i.into()), ("of", of.into()), ("data", Value::Binary(part.to_vec()))];
            if i == 0 {
                b.push(("bytes", bytes.len().into()));
                b.push(("relay", relay.into()));
            }
            self.send(to, Envelope::new("handover").body(map(b)), out);
        }
    }

    /// `moved {addrs, port, room?}` from the new host once its session runs: everyone else is told where to go. An
    /// unsealed client (the old host's in-process window) gets its ticket here; sealed ones derived theirs in the handshake.
    fn on_moved(&mut self, c: u32, env: &Envelope, out: &mut Vec<Out>) {
        if self.handover != Handover::Waiting(c) {
            return;
        }
        let addrs: Vec<String> = get_arr(&env.b, "addrs")
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|a| !a.is_empty() && a.len() <= 64)
            .take(16)
            .map(String::from)
            .collect();
        let port = get_u64(&env.b, "port").unwrap_or(0);
        if addrs.is_empty() || port == 0 || port > 65_535 {
            return;
        }
        self.handover = Handover::Done;
        let room = get_str(&env.b, "room").filter(|r| r.len() <= 64).map(String::from);
        let targets: Vec<(ConnId, bool, Vec<u8>)> = self
            .clients
            .values()
            .filter(|x| x.id != c && x.state == State::Active && x.role != Role::Replica)
            .filter_map(|x| x.conn.map(|k| (k, self.conns.get(&k).map(|cx| cx.sealed).unwrap_or(true), x.ticket.clone())))
            .collect();
        for (conn, sealed, ticket) in targets {
            let mut pairs = vec![("addrs", strs(&addrs)), ("port", port.into()), ("host", c.into())];
            if let Some(r) = &room {
                pairs.push(("room", r.as_str().into()));
            }
            if !sealed && !ticket.is_empty() {
                pairs.push(("ticket", Value::Binary(ticket)));
            }
            out.push(Out::To(conn, Envelope::new("moved").body(map(pairs))));
        }
    }

    /// Someone was removed: every remaining encrypted connection's frame keys rotate (spec § 6, LIVE-CRYPTO.md).
    fn rotate_keys(&mut self, out: &mut Vec<Out>) {
        let mut sealed: Vec<ConnId> = self.conns.iter().filter(|(_, cx)| cx.sealed).map(|(k, _)| *k).collect();
        sealed.sort();
        for conn in sealed {
            out.push(Out::Rekey(conn));
            out.push(Out::To(conn, Envelope::new("rekey")));
        }
    }

    fn end(&mut self, reason: &str, out: &mut Vec<Out>) {
        if self.ended {
            return;
        }
        let b = map(vec![("reason", reason.into()), ("q", self.seq.into())]);
        self.broadcast(&Envelope::new("end").body(b), None, out);
        for cl in self.clients.values_mut() {
            if let Some(conn) = cl.conn.take() {
                out.push(Out::Close(conn, "ended".into()));
            }
            cl.state = State::Gone;
        }
        for (conn, cx) in self.conns.drain() {
            if cx.client.is_none() {
                out.push(Out::Close(conn, "ended".into()));
            }
        }
        self.invites.clear();
        self.ended = true;
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Edits: rate → queue → checks → validate → commit

    fn on_edit(&mut self, c: u32, conn: ConnId, env: Envelope, now: Ms, out: &mut Vec<Out>) {
        if self.clients[&c].role == Role::Replica {
            return;
        }
        let cl = self.clients.get_mut(&c).unwrap();
        cl.backlog.push_back(env);
        if cl.backlog.len() > BACKLOG_REMOVE {
            self.stats.removed_flood += 1;
            out.push(Out::To(conn, Envelope::new("removed").body(map(vec![("reason", "flood".into())]))));
            self.drop_client(c, now, true, out);
            self.rotate_keys(out);
            return;
        }
        if cl.backlog.len() > BACKLOG_WARN && !cl.warned {
            cl.warned = true;
            out.push(Out::To(conn, Envelope::new("warn").body(map(vec![("reason", "flood".into())]))));
        }
        self.drain_backlog(c, now);
    }

    fn drain_backlog(&mut self, c: u32, now: Ms) {
        let cl = self.clients.get_mut(&c).unwrap();
        let dt = now.saturating_sub(cl.bucket_at) as f64 / 1000.0;
        cl.bucket = (cl.bucket + dt * RATE_PER_SEC).min(RATE_PER_SEC);
        cl.bucket_at = now;
        while cl.bucket >= 1.0 {
            let Some(env) = cl.backlog.pop_front() else { break };
            let n = env.n;
            if n <= cl.last_n {
                continue; // re-sent after a rejoin and already decided
            }
            cl.bucket -= 1.0;
            let touched = get_arr(&env.b, "touched")
                .iter()
                .take(100_000)
                .filter_map(|t| {
                    Some(Touch {
                        k: get_str(t, "k")?.to_string(),
                        f: get_str(t, "f").map(String::from),
                        r: get_u64(t, "r"),
                        m: get_str(t, "m").and_then(|s| s.chars().next()).unwrap_or('s'),
                        g: get(t, "g").map(|v| v.as_bool() == Some(true) || v.as_u64().unwrap_or(0) > 0).unwrap_or(false),
                    })
                })
                .collect();
            self.queue.push_back(Queued {
                c,
                n,
                label: get_str(&env.b, "label").unwrap_or("").chars().take(200).collect(),
                ops: get(&env.b, "ops").cloned().unwrap_or(Value::Array(vec![])),
                touched,
            });
            cl.last_n = n;
        }
        if cl.backlog.len() < BACKLOG_WARN / 2 {
            cl.warned = false;
        }
    }

    fn pump(&mut self, out: &mut Vec<Out>) {
        if self.handover != Handover::None {
            return; // handing over: the new host decides what is still queued
        }
        while self.validating.is_none() {
            let Some(e) = self.queue.pop_front() else { return };
            if let Err((reason, by)) = self.check(&e) {
                self.refuse(&e, reason, by, out);
                continue;
            }
            match self.replica {
                Some(r) => {
                    let b = map(vec![
                        ("c", e.c.into()),
                        ("n", e.n.into()),
                        ("label", e.label.clone().into()),
                        ("ops", e.ops.clone()),
                    ]);
                    let mut env = Envelope::new("validate").body(b);
                    env.q = self.seq + 1;
                    self.send(r, env, out);
                    self.validating = Some(e);
                }
                None => self.commit(e, out),
            }
        }
    }

    fn on_valid(&mut self, env: &Envelope, out: &mut Vec<Out>) {
        let Some(e) = self.validating.take() else { return };
        if self.handover != Handover::None {
            self.queue.push_front(e); // left undecided: its author re-sends it to the new host
            return;
        }
        if get_u64(&env.b, "n") != Some(e.n) || get_u64(&env.b, "c") != Some(e.c as u64) {
            // Out of step: the replica answered something else. Refuse rather than guess.
            self.refuse(&e, "invalid".into(), None, out);
            return;
        }
        if get(&env.b, "ok").and_then(|v| v.as_bool()) == Some(true) {
            self.commit(e, out);
        } else {
            let reason = get_str(&env.b, "reason").unwrap_or("invalid").chars().take(200).collect();
            self.refuse(&e, reason, None, out);
        }
    }

    /// Server-side rules, in order: role, locks, holds, versions (LIVE-PROTOCOL.md § 6.2).
    fn check(&self, e: &Queued) -> Result<(), (String, Option<u32>)> {
        let Some(cl) = self.clients.get(&e.c) else { return Err(("gone".into(), None)) };
        if !cl.role.can_edit() {
            return Err(("role".into(), None));
        }
        if e.touched.iter().any(|t| self.locks.contains(&t.k)) {
            return Err(("locked".into(), None));
        }
        for t in &e.touched {
            if matches!(t.m, 's' | 'd') || (t.m == 'r' && t.g) {
                if let Some((h, _)) = self.holds.get(&t.k) {
                    if *h != e.c {
                        return Err(("held".into(), Some(*h)));
                    }
                }
            }
        }
        for t in &e.touched {
            let ks = self.keys.get(&t.k);
            let deleted = ks.map(|k| k.deleted).unwrap_or(false);
            let by = ks.map(|k| k.by);
            match t.m {
                'p' => {
                    if deleted {
                        return Err(("deleted".into(), by));
                    }
                    if let (Some(r), Some(f)) = (t.r, &t.f) {
                        let (pr, pby) = self.props.get(&prop_key(&t.k, f)).copied().unwrap_or((0, 0));
                        if pr != r {
                            return Err(("changed".into(), Some(pby).filter(|x| *x != 0)));
                        }
                    }
                }
                'c' => {
                    if ks.is_some() && !deleted {
                        return Err(("exists".into(), by));
                    }
                }
                'd' => {
                    if deleted {
                        return Err(("deleted".into(), by));
                    }
                    // Delete wins over property writes; a condition, when given, is on structure only.
                    if let Some(r) = t.r {
                        if self.struct_rev.get(&t.k).copied().unwrap_or(0) > r {
                            return Err(("changed".into(), by));
                        }
                    }
                }
                _ => {
                    // 's' structural write and 'r' read: the key must not be deleted and, with r, be unchanged.
                    if deleted {
                        return Err(("deleted".into(), by));
                    }
                    if let Some(r) = t.r {
                        if ks.map(|k| k.rev).unwrap_or(0) != r {
                            return Err(("changed".into(), by));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn refuse(&mut self, e: &Queued, reason: String, by: Option<u32>, out: &mut Vec<Out>) {
        self.stats.refused += 1;
        let mut pairs = vec![("n", Value::from(e.n)), ("reason", Value::from(reason))];
        if let Some(by) = by {
            pairs.push(("by", by.into()));
            if let Some(p) = self.clients.get(&by) {
                pairs.push(("by_name", p.name.clone().into()));
            }
        }
        let body = map(pairs);
        if let Some(cl) = self.clients.get_mut(&e.c) {
            cl.refusals.push_back((e.n, body.clone()));
            while cl.refusals.len() > 1000 {
                cl.refusals.pop_front();
            }
        }
        let mut env = Envelope::new("refuse").body(body);
        env.n = e.n;
        self.send(e.c, env, out);
    }

    fn commit(&mut self, e: Queued, out: &mut Vec<Out>) {
        self.seq += 1;
        let q = self.seq;
        for t in &e.touched {
            match t.m {
                'r' => {}
                'p' => {
                    let ks = self.keys.entry(t.k.clone()).or_default();
                    ks.rev = q;
                    ks.by = e.c;
                    if let Some(f) = &t.f {
                        self.props.insert(prop_key(&t.k, f), (q, e.c));
                    }
                }
                m => {
                    let ks = self.keys.entry(t.k.clone()).or_default();
                    ks.rev = q;
                    ks.by = e.c;
                    ks.deleted = m == 'd';
                    if m == 'c' {
                        // A re-created key (undo of a delete) starts its properties afresh.
                        let pre = format!("{}\u{0}", t.k);
                        self.props.retain(|k, _| !k.starts_with(&pre));
                    }
                    self.struct_rev.insert(t.k.clone(), q);
                    if m == 'd' && self.holds.remove(&t.k).is_some() {
                        self.broadcast(&holds_change(&[], std::slice::from_ref(&t.k)), None, out);
                    }
                }
            }
        }
        let touched = Value::Array(
            e.touched
                .iter()
                .map(|t| {
                    let mut p = vec![("k", Value::from(t.k.as_str())), ("m", Value::from(t.m.to_string()))];
                    if let Some(f) = &t.f {
                        p.push(("f", f.as_str().into()));
                    }
                    map(p)
                })
                .collect(),
        );
        let cm = Commit { q, c: e.c, n: e.n, label: e.label, ops: e.ops, touched };
        self.stats.commits += 1;
        let env = commit_env(&cm);
        self.broadcast(&env, None, out);
        self.log.push_back(cm);
        while self.log.len() > self.cfg.log_keep {
            self.log.pop_front();
        }
    }

    fn on_snap(&mut self, env: Envelope, out: &mut Vec<Out>) {
        // The replica (or host) answers `snapreq` with the whole snapshot: {for, q, data: bin, enc}. We chunk it.
        let Some(target) = self.snap_busy else { return };
        if get_u64(&env.b, "for") != Some(target as u64) {
            return;
        }
        let q = get_u64(&env.b, "q").unwrap_or(self.seq).min(self.seq);
        let data: &[u8] = get(&env.b, "data").and_then(bin).unwrap_or(&[]);
        let enc = get_str(&env.b, "enc").unwrap_or("").to_string();
        // The keys' versions as of the snapshot (`revs`, before its chunks): without them a snapshot joiner's structural edit on
        // anything written before it joined names version 0 and is refused `changed` (found in the 0.5 final sweep).
        let mut known: Vec<(&String, u64)> = self.keys.iter().filter(|(_, k)| k.rev > 0 && k.rev <= q).map(|(k, v)| (k, v.rev)).collect();
        known.sort();
        let revs: Vec<Envelope> = known
            .chunks(REVS_CHUNK)
            .map(|part| {
                let m: Vec<(Value, Value)> = part.iter().map(|(k, r)| (Value::from(k.as_str()), Value::from(*r))).collect();
                let mut e = Envelope::new("revs").body(map(vec![("keys", Value::Map(m))]));
                e.q = q;
                e
            })
            .collect();
        for e in revs {
            self.send(target, e, out);
        }
        let total = data.len().div_ceil(SNAPSHOT_CHUNK).max(1);
        let mut chunks = Vec::with_capacity(total);
        for i in 0..total {
            let part = &data[(i * SNAPSHOT_CHUNK).min(data.len())..((i + 1) * SNAPSHOT_CHUNK).min(data.len())];
            let mut pairs = vec![("i", Value::from(i)), ("of", Value::from(total)), ("data", Value::Binary(part.to_vec()))];
            if i == 0 {
                pairs.push(("enc", enc.clone().into()));
                pairs.push(("bytes", data.len().into()));
            }
            let mut e = Envelope::new("snap").body(map(pairs));
            e.q = q;
            chunks.push(e);
        }
        for e in chunks {
            self.send(target, e, out);
        }
        // Commits after q follow the snapshot.
        let after: Vec<Envelope> = self.log.iter().filter(|x| x.q > q).map(commit_env).collect();
        for e in after {
            self.send(target, e, out);
        }
        self.snap_busy = None;
        self.pump_snapshots(out);
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Time

    pub fn tick(&mut self, now: Ms) -> Vec<Out> {
        self.now = now;
        let mut out = vec![];
        if self.ended {
            return out;
        }
        // Rate-limited backlogs.
        let ids: Vec<u32> = self.clients.iter().filter(|(_, c)| !c.backlog.is_empty()).map(|(id, _)| *id).collect();
        for c in ids {
            self.drain_backlog(c, now);
        }
        self.pump(&mut out);
        // Presence fan-out.
        if now.saturating_sub(self.last_presence) >= PRESENCE_EVERY {
            self.last_presence = now;
            let dirty: Vec<(u32, Value)> = self
                .clients
                .values_mut()
                .filter(|c| c.presence_dirty && c.state == State::Active)
                .map(|c| {
                    c.presence_dirty = false;
                    (c.id, c.presence.clone())
                })
                .collect();
            if !dirty.is_empty() {
                let targets: Vec<(u32, ConnId)> = self
                    .clients
                    .values()
                    .filter(|c| c.state == State::Active && c.role != Role::Replica)
                    .filter_map(|c| c.conn.map(|k| (c.id, k)))
                    .collect();
                for (id, conn) in targets {
                    let peers: Vec<(Value, Value)> =
                        dirty.iter().filter(|(c, _)| *c != id).map(|(c, v)| (Value::from(*c), v.clone())).collect();
                    if peers.is_empty() {
                        continue;
                    }
                    self.stats.presence_out += 1;
                    out.push(Out::To(conn, Envelope::new("presence").body(map(vec![("peers", Value::Map(peers))]))));
                }
            }
        }
        // Pings, and 3 missed = gone (holds released, presence greyed).
        if now.saturating_sub(self.last_ping) >= PING_EVERY {
            self.last_ping = now;
            let conns: Vec<ConnId> = self.conns.iter().filter(|(_, cx)| cx.client.is_some()).map(|(k, _)| *k).collect();
            for conn in conns {
                out.push(Out::To(conn, Envelope { t: "ping".into(), n: now, ..Default::default() }));
            }
        }
        let late: Vec<u32> = self
            .clients
            .values()
            .filter(|c| c.state != State::Gone && c.conn.is_some() && now.saturating_sub(c.last_seen) > GONE_AFTER)
            .filter(|c| !c.conn.map(|k| self.conns.get(&k).map(|x| x.trusted).unwrap_or(false)).unwrap_or(false))
            .map(|c| c.id)
            .collect();
        for c in late {
            self.drop_client(c, now, false, &mut out);
        }
        // Handshakes that never proved the code, and connections that never said hello.
        let stale: Vec<(ConnId, Option<String>)> = self
            .conns
            .iter()
            .filter(|(_, cx)| cx.client.is_none() && !cx.trusted)
            .filter(|(_, cx)| match &cx.hs {
                Some(h) => now.saturating_sub(h.started) > HANDSHAKE_TIMEOUT,
                None => now.saturating_sub(cx.opened) > HANDSHAKE_TIMEOUT,
            })
            .map(|(k, cx)| (*k, cx.hs.as_ref().map(|h| h.kid.clone())))
            .collect();
        for (conn, kid) in stale {
            self.conns.remove(&conn);
            out.push(Out::Close(conn, "timeout".into()));
            if let Some(kid) = kid {
                self.wrong_try(&kid, now, &mut out);
            }
        }
        // Hold expiry.
        let expired: Vec<String> = self.holds.iter().filter(|(_, (_, t))| *t < now).map(|(k, _)| k.clone()).collect();
        if !expired.is_empty() {
            for k in &expired {
                self.holds.remove(k);
            }
            self.broadcast(&holds_change(&[], &expired), None, &mut out);
        }
        // Code expiry.
        let old: Vec<String> = self.invites.iter().filter(|(_, k)| k.expires <= now).map(|(k, _)| k.clone()).collect();
        for k in old {
            let quiet = matches!(self.invites.remove(&k), Some(Key { kind: KeyKind::Assistant(_), .. }));
            if !quiet {
                self.to_role(Role::Host, code_gone(&k, "expired"), &mut out);
            }
        }
        // Forget dropped clients after the rejoin window.
        let forgotten: Vec<u32> =
            self.clients.values().filter(|c| c.state == State::Gone && now.saturating_sub(c.gone_at) > REJOIN_WINDOW).map(|c| c.id).collect();
        for c in forgotten {
            self.clients.remove(&c);
            self.invites.remove(&ticket_kid(c));
        }
        out
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Sending

    fn send(&self, c: u32, env: Envelope, out: &mut Vec<Out>) {
        if let Some(conn) = self.clients.get(&c).and_then(|x| x.conn) {
            out.push(Out::To(conn, env));
        }
    }
    fn role_client(&self, role: Role) -> Option<u32> {
        self.clients.values().find(|c| c.role == role && c.state == State::Active).map(|c| c.id)
    }
    fn to_role(&self, role: Role, env: Envelope, out: &mut Vec<Out>) {
        for c in self.clients.values().filter(|c| c.role == role && c.state == State::Active) {
            if let Some(conn) = c.conn {
                out.push(Out::To(conn, env.clone()));
            }
        }
    }
    /// To every active client (the replica too: it needs commits, holds and locks).
    fn broadcast(&self, env: &Envelope, except: Option<u32>, out: &mut Vec<Out>) {
        for c in self.clients.values().filter(|c| c.state == State::Active && Some(c.id) != except) {
            if let Some(conn) = c.conn {
                out.push(Out::To(conn, env.clone()));
            }
        }
    }
}

fn code_gone(kid: &str, reason: &str) -> Envelope {
    Envelope::new("code-gone").body(map(vec![("kid", kid.into()), ("reason", reason.into())]))
}
