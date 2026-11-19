//! Sans-IO IRC client session.
//!
//! Feed it parsed [`Message`]s with [`Session::handle`]; collect lines to send with
//! [`Session::take_outbox`] and UI events with [`Session::take_events`]. It never
//! touches a socket or a clock, so it is fully unit-testable.

use crate::config::Config;
use crate::event::{ChatKind, Event, HistoryLine, ModeChange, TypingState};
use crate::scram::{self, Scram};
use crate::presence;
use crate::sts;
use crate::ignore::{IgnoreKind, IgnoreRule};
use crate::isupport::ISupport;
use crate::state::{Channel, Member};
use base64::Engine;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;
use chimerairc_proto::{Ctcp, Message, Source};

/// Capabilities we ask for when the server offers them. Every one listed here
/// is actually handled below; do not add a cap without handling its effects.
const WANTED_CAPS: &[&str] = &[
    "multi-prefix",
    "server-time",
    "message-tags",
    "echo-message",
    "extended-join",
    "userhost-in-names",
    "away-notify",
    "account-notify",
    "chghost",
    "batch",
    "setname",
    "invite-notify",
    "labeled-response",
];

/// The chat history capability, under its final name and its draft one. Only asked for when the
/// server also offers `batch`, because the history arrives in batches.
const HISTORY_CAPS: [&str; 2] = ["chathistory", "draft/chathistory"];

/// Most lines kept from one history batch, and most batches open at once: a server (or a bug) cannot
/// make us buffer without limit.
const MAX_BATCH_LINES: usize = 2000;
const MAX_OPEN_BATCHES: usize = 8;

/// A `chathistory` batch being collected.
struct Batch {
    target: String,
    lines: Vec<HistoryLine>,
}

/// A netsplit or netjoin batch being collected: the quits (or joins) inside it are summed up in one event.
struct Split {
    join: bool,
    servers: Vec<String>,
    nicks: Vec<String>,
    channels: Vec<String>,
}

/// Most names kept from one netsplit batch.
const MAX_SPLIT_NICKS: usize = 5000;

/// Which SASL mechanism is running, and where it is.
enum Mech {
    Plain,
    /// EXTERNAL: the TLS client certificate is the credential.
    External,
    /// SCRAM-SHA-256: the exchange, and what has been seen of it so far.
    Scram { state: Box<Scram>, stage: ScramStage },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScramStage {
    /// We sent the mechanism name and wait for the server's `+`.
    WantGo,
    /// We sent our first message and wait for the server's.
    WantFirst,
    /// We sent the proof and wait for the server's signature.
    WantFinal,
    /// The server proved itself; only the 903 is left.
    Verified,
}

/// The mechanism to use given the server's `sasl=` list (empty when it gave none).
///
/// SCRAM-SHA-256 wins whenever it is offered and the credentials can use it. PLAIN is the fallback,
/// and the answer when the server named no mechanisms. If SCRAM was offered and fails, the login
/// fails: we never quietly retry with PLAIN, which would hand the password to whoever made SCRAM fail.
fn choose_mech(offered: &str, scram_ok: bool, external_ok: bool, have_creds: bool) -> Option<&'static str> {
    let has = |m: &str| offered.split(',').any(|x| x.trim().eq_ignore_ascii_case(m));
    // A client certificate is the strongest credential there is: use it whenever the server accepts it.
    if external_ok && has("EXTERNAL") {
        return Some("EXTERNAL");
    }
    if !have_creds {
        // with no password to fall back on, a server that listed no mechanisms gets an EXTERNAL try
        return (external_ok && offered.is_empty()).then_some("EXTERNAL");
    }
    if offered.is_empty() {
        return Some("PLAIN");
    }
    if has(scram::MECHANISM) && scram_ok {
        Some(scram::MECHANISM)
    } else if has("PLAIN") {
        Some("PLAIN")
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sasl {
    Idle,
    InProgress,
    Done,
}

pub struct Session {
    cfg: Config,
    nick: String,
    registered: bool,
    negotiating: bool,
    ls_complete: bool,
    available: HashMap<String, String>,
    pending: HashSet<String>,
    enabled: HashSet<String>,
    sasl: Sasl,
    sasl_buf: String,
    /// The running SASL mechanism, and the server's AUTHENTICATE data collected so far.
    mech: Option<Mech>,
    sasl_in: String,
    /// `chathistory` batches in progress, by reference tag.
    batches: HashMap<String, Batch>,
    /// Open `labeled-response` batches: batch reference -> the label of the command they answer.
    labeled: HashMap<String, String>,
    /// 
    /// netsplit / netjoin batches in progress, by reference tag.
    splits: HashMap<String, Split>,
    /// How many of `cfg.alt_nicks` were tried.
    alt_used: usize,
    /// Registration (PASS, NICK, USER) has been sent.
    registration_sent: bool,
    /// The STS policy of this server has been looked at (once per connection).
    sts_done: bool,
    /// The server asked for TLS: this plaintext session is finished and ignores everything.
    upgrading: bool,
    pub isupport: ISupport,
    channels: HashMap<String, Channel>,
    /// Channels whose member list changed since the last `take_events`.
    dirty: HashSet<String>,
    ignores: Vec<IgnoreRule>,
    outbox: Vec<Message>,
    events: Vec<Event>,
    /// When (on `cfg.monotonic`) the automatic CTCP replies of the current rate-limit window went out.
    reply_times: VecDeque<Duration>,
    /// Nicknames being watched (as given), the last known state of each (by folded name), the ones
    /// registered with MONITOR, the ISON chunks still waiting for their 303, and when ISON last went out.
    watch: Vec<String>,
    presence: HashMap<String, bool>,
    monitored: HashSet<String>,
    ison_pending: VecDeque<Vec<String>>,
    last_ison: Option<Duration>,
    /// The end of the MOTD has been seen: ISUPPORT is complete, so the watch list can be sent.
    watch_ready: bool,
}

/// How often watched nicknames are polled with ISON when the server has no MONITOR.
const ISON_EVERY: Duration = Duration::from_secs(60);

impl Session {
    pub fn new(cfg: Config) -> Session {
        Session {
            nick: cfg.nick.clone(),
            ignores: cfg.ignores.clone(),
            cfg,
            registered: false,
            negotiating: false,
            ls_complete: false,
            available: HashMap::new(),
            pending: HashSet::new(),
            enabled: HashSet::new(),
            sasl: Sasl::Idle,
            sasl_buf: String::new(),
            mech: None,
            sasl_in: String::new(),
            batches: HashMap::new(),
            labeled: HashMap::new(),
            splits: HashMap::new(),
            alt_used: 0,
            registration_sent: false,
            sts_done: false,
            upgrading: false,
            isupport: ISupport::default(),
            channels: HashMap::new(),
            dirty: HashSet::new(),
            outbox: Vec::new(),
            events: Vec::new(),
            reply_times: VecDeque::new(),
            watch: Vec::new(),
            presence: HashMap::new(),
            monitored: HashSet::new(),
            ison_pending: VecDeque::new(),
            last_ison: None,
            watch_ready: false,
        }
    }

    pub fn nick(&self) -> &str {
        &self.nick
    }
    pub fn is_registered(&self) -> bool {
        self.registered
    }
    pub fn cap_enabled(&self, cap: &str) -> bool {
        self.enabled.contains(cap)
    }
    pub fn is_me(&self, nick: &str) -> bool {
        self.fold(nick) == self.fold(&self.nick)
    }
    pub fn channel(&self, name: &str) -> Option<&Channel> {
        self.channels.get(&self.fold(name))
    }
    pub fn channels(&self) -> impl Iterator<Item = &Channel> {
        self.channels.values()
    }
    /// Replace the ignore rules. Takes effect for the next message.
    pub fn set_ignores(&mut self, rules: Vec<IgnoreRule>) {
        self.ignores = rules;
    }

    /// Replace the list of nicknames whose online state is wanted. Names that are not valid
    /// nicknames, duplicates (by the server's case rules) and anything beyond 500 are dropped.
    pub fn set_watch(&mut self, nicks: &[String]) {
        let mut seen = HashSet::new();
        let mut list = Vec::new();
        for n in nicks {
            if presence::valid_nick(n) && list.len() < presence::MAX_WATCH && seen.insert(self.fold(n)) {
                list.push(n.clone());
            }
        }
        self.presence.retain(|k, _| seen.contains(k));
        self.watch = list;
        if self.watch_ready {
            self.sync_watch();
        }
    }

    /// Tell the server what to report: MONITOR where it exists (up to its limit), ISON for the rest.
    fn sync_watch(&mut self) {
        if !self.monitored.is_empty() {
            self.send("MONITOR", &["C"]);
            self.monitored.clear();
        }
        self.ison_pending.clear();
        self.last_ison = None;
        let Some(limit) = self.isupport.raw.get("MONITOR").map(|v| v.parse::<usize>().unwrap_or(0)) else {
            return;
        };
        let limit = if limit == 0 { usize::MAX } else { limit };
        let take: Vec<String> = self.watch.iter().take(limit).cloned().collect();
        for n in &take {
            let k = self.fold(n);
            self.monitored.insert(k);
        }
        for g in presence::chunks(&take, presence::CHUNK_BYTES) {
            let list = g.join(",");
            self.send("MONITOR", &["+", &list]);
        }
    }

    /// Poll the watched nicknames that MONITOR does not cover. Call about once a second.
    pub fn poll(&mut self) {
        let now = (self.cfg.monotonic)();
        self.poll_at(now);
    }

    pub fn poll_at(&mut self, now: Duration) {
        if !self.registered || !self.watch_ready || !self.ison_pending.is_empty() {
            return;
        }
        if self.last_ison.is_some_and(|t| now.saturating_sub(t) < ISON_EVERY) {
            return;
        }
        let rest: Vec<String> =
            self.watch.iter().filter(|n| !self.monitored.contains(&self.fold(n))).cloned().collect();
        if rest.is_empty() {
            return;
        }
        self.last_ison = Some(now);
        for g in presence::chunks(&rest, presence::CHUNK_BYTES) {
            let list = g.join(" ");
            self.send("ISON", &[&list]);
            self.ison_pending.push_back(g);
        }
    }

    fn set_presence(&mut self, nick: &str, online: bool, mask: Option<String>) {
        let key = self.fold(nick);
        let Some(shown) = self.watch.iter().find(|n| self.fold(n) == key).cloned() else { return };
        let prev = self.presence.insert(key, online);
        if prev != Some(online) {
            self.emit(Event::Presence { nick: shown, online, mask, announce: prev.is_some() });
        }
    }

    fn on_presence_numeric(&mut self, code: u16, m: &Message) {
        match code {
            // RPL_MONONLINE / RPL_MONOFFLINE: [me, "nick!user@host,nick2"]
            730 | 731 => {
                let Some(list) = m.params.get(1) else { return };
                for t in list.split(',').filter(|t| !t.is_empty()) {
                    let nick = t.split('!').next().unwrap_or(t);
                    let mask = (code == 730 && t.contains('!')).then(|| t.to_owned());
                    self.set_presence(nick, code == 730, mask);
                }
            }
            // ERR_MONLISTFULL: [me, limit, targets, text]: those are polled instead
            734 => {
                if let Some(list) = m.params.get(2) {
                    for t in list.split(',') {
                        let k = self.fold(t);
                        self.monitored.remove(&k);
                    }
                    self.last_ison = None;
                }
            }
            // RPL_ISON: [me, "nick nick2"], answering our oldest ISON
            303 => {
                let Some(chunk) = self.ison_pending.pop_front() else {
                    self.emit(Event::Numeric { code, params: m.params.iter().skip(1).cloned().collect() });
                    return;
                };
                let on: HashSet<String> = m
                    .params
                    .get(1)
                    .map(|l| l.split_whitespace().map(|n| self.fold(n)).collect())
                    .unwrap_or_default();
                for n in chunk {
                    let online = on.contains(&self.fold(&n));
                    self.set_presence(&n, online, None);
                }
            }
            _ => {}
        }
    }

    fn is_ignored(&self, src: &Source, target: &str, kind: IgnoreKind) -> bool {
        // Our own echoed messages are never ignored, even by a catch-all mask.
        !self.is_me(&src.name) && self.ignores.iter().any(|r| r.applies(&self.cfg.host, src, target, kind))
    }

    pub fn take_outbox(&mut self) -> Vec<Message> {
        std::mem::take(&mut self.outbox)
    }
    /// Drain events. One `Event::Members` snapshot is appended per channel whose
    /// member list changed, so a burst of joins/quits costs one update per drain.
    pub fn take_events(&mut self) -> Vec<Event> {
        let mut out = std::mem::take(&mut self.events);
        let mut keys: Vec<String> = self.dirty.drain().collect();
        keys.sort();
        for k in keys {
            if let Some(ch) = self.channels.get(&k) {
                out.push(Event::Members { channel: ch.name.clone(), members: self.snapshot(ch) });
            }
        }
        out
    }

    fn snapshot(&self, ch: &Channel) -> Vec<(String, String)> {
        let rank = |m: &Member| {
            self.isupport.prefix.iter().position(|(l, _)| m.modes.contains(*l)).unwrap_or(usize::MAX)
        };
        let mut v: Vec<&Member> = ch.members.values().collect();
        v.sort_by_cached_key(|m| (rank(m), m.nick.to_lowercase()));
        v.into_iter()
            .map(|m| {
                let sym = self
                    .isupport
                    .prefix
                    .iter()
                    .find(|(l, _)| m.modes.contains(*l))
                    .map(|(_, s)| s.to_string())
                    .unwrap_or_default();
                (sym, m.nick.clone())
            })
            .collect()
    }

    fn touch(&mut self, key: &str) {
        if self.channels.get(key).is_some_and(|c| c.names_complete) {
            self.dirty.insert(key.to_owned());
        }
    }

    fn fold(&self, s: &str) -> String {
        self.isupport.casemapping.fold(s)
    }
    fn send(&mut self, cmd: &str, params: &[&str]) {
        self.outbox.push(Message::new(cmd, params));
    }
    fn emit(&mut self, e: Event) {
        self.events.push(e);
    }

    /// Begin registration. Call once after the transport is up.
    pub fn start(&mut self) {
        self.negotiating = true;
        self.send("CAP", &["LS", "302"]);
        // On a plaintext connection nothing private goes out until the server has answered CAP LS:
        // it may ask for TLS (STS), and then the password must never have been sent in the clear.
        if self.holds_registration() {
            return;
        }
        self.send_registration();
    }

    fn holds_registration(&self) -> bool {
        !self.cfg.tls && (self.cfg.password.is_some() || self.cfg.sasl.is_some())
    }

    fn send_registration(&mut self) {
        if self.registration_sent {
            return;
        }
        self.registration_sent = true;
        if let Some(p) = self.cfg.password.clone() {
            self.send("PASS", &[&p]);
        }
        let nick = self.nick.clone();
        self.send("NICK", &[&nick]);
        let (user, real) = (self.cfg.user.clone(), self.cfg.realname.clone());
        self.send("USER", &[&user, "0", "*", &real]);
    }

    /// True once the server has asked for a TLS connection instead (STS); the transport ends this
    /// session then.
    pub fn is_upgrading(&self) -> bool {
        self.upgrading
    }

    /// Look at the server's `sts` capability, once. Over TLS it is a policy to remember; over
    /// plaintext with a port it is a demand to reconnect securely.
    fn check_sts(&mut self) {
        if self.sts_done {
            return;
        }
        let Some(value) = self.available.get("sts").cloned() else { return };
        self.sts_done = true;
        let policy = sts::parse(&value);
        if self.cfg.tls {
            if let Some(duration) = policy.duration {
                let port = self.cfg.port;
                self.emit(Event::StsPolicy { duration, port, preload: policy.preload });
            }
        } else if let Some(port) = policy.port {
            self.upgrading = true;
            self.emit(Event::StsUpgrade { port });
        }
    }

    pub fn handle(&mut self, m: &Message) {
        if self.upgrading {
            return; // finished: the connection is being replaced by a secure one
        }
        // `labeled-response`: what the server sends back for a command of ours carries its label (on the message, or
        // on the batch the message is in); the UI is told which events belong to which command.
        let label = m.tag("label").map(str::to_owned).or_else(|| m.tag("batch").and_then(|r| self.labeled.get(r).cloned()));
        match label {
            Some(l) => {
                self.emit(Event::Label { label: Some(l) });
                self.handle_one(m);
                self.emit(Event::Label { label: None });
            }
            None => self.handle_one(m),
        }
    }

    fn handle_one(&mut self, m: &Message) {
        let cmd = m.command.as_str();
        match cmd {
            "PING" => {
                let p: Vec<&str> = m.params.iter().map(String::as_str).collect();
                self.send("PONG", &p);
            }
            "CAP" => self.on_cap(m),
            "AUTHENTICATE" => self.on_authenticate(m),
            "BATCH" => self.on_batch(m),
            "ERROR" => {
                let text = m.params.first().cloned().unwrap_or_default();
                self.emit(Event::Error { text });
            }
            "PRIVMSG" | "NOTICE" => self.on_chat(m),
            "JOIN" => self.on_join(m),
            "PART" => self.on_part(m),
            "QUIT" => self.on_quit(m),
            "KICK" => self.on_kick(m),
            "NICK" => self.on_nick(m),
            "TOPIC" => self.on_topic(m),
            "MODE" => self.on_mode(m),
            "TAGMSG" => self.on_tagmsg(m),
            "INVITE" => self.on_invite(m),
            "SETNAME" => {
                if let (Some(s), Some(realname)) = (&m.source, m.params.first()) {
                    self.emit(Event::SetName { nick: s.name.clone(), realname: realname.clone() });
                }
            }
            "AWAY" => {
                if let Some(s) = &m.source {
                    let message = m.params.first().cloned();
                    self.emit(Event::Away { nick: s.name.clone(), message });
                }
            }
            "ACCOUNT" => {
                if let Some(s) = &m.source {
                    let account = m.params.first().filter(|a| *a != "*").cloned();
                    self.emit(Event::Account { nick: s.name.clone(), account });
                }
            }
            "CHGHOST" => {
                if let (Some(s), [user, host, ..]) = (&m.source, m.params.as_slice()) {
                    let ev = Event::ChgHost { nick: s.name.clone(), user: user.clone(), host: host.clone() };
                    self.emit(ev);
                }
            }
            _ if m.is_numeric() => self.on_numeric(m),
            _ => {}
        }
    }

    // ---- CAP / SASL -------------------------------------------------------

    fn on_cap(&mut self, m: &Message) {
        let sub = m.params.get(1).map(|s| s.to_ascii_uppercase()).unwrap_or_default();
        // LS may be multiline: `CAP * LS * :caps` then `CAP * LS :caps`.
        let (more, list) = match m.params.as_slice() {
            [_, _, star, list] if star == "*" => (true, list.as_str()),
            [_, _, list] => (false, list.as_str()),
            _ => (false, ""),
        };
        match sub.as_str() {
            "LS" | "NEW" => {
                for item in list.split(' ').filter(|s| !s.is_empty()) {
                    let (k, v) = item.split_once('=').unwrap_or((item, ""));
                    self.available.insert(k.to_owned(), v.to_owned());
                }
                self.check_sts();
                if self.upgrading {
                    return;
                }
                if sub == "LS" {
                    if !more {
                        self.ls_complete = true;
                        self.send_registration(); // held back on plaintext until here
                        self.request_caps();
                    }
                } else {
                    self.request_caps(); // post-registration CAP NEW
                }
            }
            "ACK" => {
                for item in list.split(' ').filter(|s| !s.is_empty()) {
                    if let Some(c) = item.strip_prefix('-') {
                        self.enabled.remove(c);
                    } else {
                        self.pending.remove(item);
                        self.enabled.insert(item.to_owned());
                        if item == "sasl" {
                            self.start_sasl();
                        }
                    }
                }
                self.check_cap_end();
            }
            "NAK" => {
                for item in list.split(' ').filter(|s| !s.is_empty()) {
                    self.pending.remove(item);
                    if item == "sasl" {
                        self.emit(Event::SaslResult { ok: false, text: "server refused sasl".into() });
                    }
                }
                self.check_cap_end();
            }
            "DEL" => {
                for item in list.split(' ').filter(|s| !s.is_empty()) {
                    self.available.remove(item);
                    self.enabled.remove(item);
                }
            }
            _ => {}
        }
    }

    fn request_caps(&mut self) {
        let mut want: Vec<String> = WANTED_CAPS
            .iter()
            .filter(|c| self.available.contains_key(**c) && !self.enabled.contains(**c))
            .map(|c| (*c).to_owned())
            .collect();
        // History needs batches (and the other end's server-time to be of any use): ask for the
        // first of its two names that the server offers, but only with `batch` in hand.
        let batch_ok = self.enabled.contains("batch") || want.iter().any(|c| c == "batch");
        if batch_ok && self.cfg.history_on_join > 0 && !HISTORY_CAPS.iter().any(|c| self.enabled.contains(*c)) {
            if let Some(c) = HISTORY_CAPS.iter().find(|c| self.available.contains_key(**c)) {
                want.push((*c).to_owned());
            }
        }
        let external_possible = self.cfg.tls && self.cfg.client_cert.is_some();
        if (self.cfg.sasl.is_some() || external_possible) && self.negotiating && !self.enabled.contains("sasl") {
            match self.sasl_mechanism() {
                Some(_) => want.push("sasl".into()),
                None => self.emit(Event::SaslResult {
                    ok: false,
                    text: if self.cfg.sasl.is_some() {
                        "server offers none of EXTERNAL, SCRAM-SHA-256 and PLAIN that we can use".into()
                    } else {
                        "server does not offer SASL EXTERNAL".into()
                    },
                }),
            }
        }
        // One REQ per cap so a single NAK cannot sink the rest.
        for c in want {
            self.pending.insert(c.clone());
            self.send("CAP", &["REQ", &c]);
        }
        self.check_cap_end();
    }

    fn check_cap_end(&mut self) {
        if self.negotiating && self.ls_complete && self.pending.is_empty() && self.sasl != Sasl::InProgress {
            self.negotiating = false;
            self.send("CAP", &["END"]);
            let mut enabled: Vec<String> = self.enabled.iter().cloned().collect();
            enabled.sort();
            self.emit(Event::CapsEnabled { enabled });
        }
    }

    /// The mechanism for the credentials we hold and what the server listed (None: no way in).
    fn sasl_mechanism(&self) -> Option<&'static str> {
        let offered = self.available.get("sasl").map(String::as_str).unwrap_or("");
        let external_ok = self.cfg.tls && self.cfg.client_cert.is_some();
        match &self.cfg.sasl {
            Some((acct, pass)) => choose_mech(offered, scram::can_use(acct, pass), external_ok, true),
            None => choose_mech(offered, false, external_ok, false),
        }
    }

    fn start_sasl(&mut self) {
        let Some(name) = self.sasl_mechanism() else { return };
        self.sasl = Sasl::InProgress;
        self.sasl_buf.clear();
        self.sasl_in.clear();
        if name == "EXTERNAL" {
            self.mech = Some(Mech::External);
        } else if name == scram::MECHANISM {
            let (acct, pass) = self.cfg.sasl.clone().unwrap_or_default();
            let nonce = (self.cfg.scram_nonce)();
            self.mech = Some(Mech::Scram { state: Box::new(Scram::new(&acct, &pass, &nonce)), stage: ScramStage::WantGo });
        } else {
            self.mech = Some(Mech::Plain);
        }
        self.send("AUTHENTICATE", &[name]);
    }

    /// Send `data` as base64 `AUTHENTICATE` lines of at most 400 characters; a payload that is an
    /// exact multiple of 400 is closed with `+`.
    fn send_authenticate_payload(&mut self, data: &[u8]) {
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        let mut rest = b64.as_str();
        loop {
            let (head, tail) = rest.split_at(rest.len().min(400));
            self.send("AUTHENTICATE", &[head]);
            rest = tail;
            if head.len() < 400 {
                break;
            }
            if rest.is_empty() {
                self.send("AUTHENTICATE", &["+"]);
                break;
            }
        }
    }

    /// Give up on the login: tell the server (`AUTHENTICATE *`) and report why.
    fn abort_sasl(&mut self, why: String) {
        self.send("AUTHENTICATE", &["*"]);
        self.mech = None;
        self.finish_sasl(false, why);
    }

    fn on_authenticate(&mut self, m: &Message) {
        if self.sasl != Sasl::InProgress {
            return;
        }
        let Some(chunk) = m.params.first() else { return };
        // The server's data arrives in lines of at most 400 characters: a full line means more
        // follows, a shorter one (or a lone "+") ends it.
        if chunk != "+" {
            if self.sasl_in.len() + chunk.len() > 16 * 1024 {
                return self.abort_sasl("the server's authentication data is too long".into());
            }
            self.sasl_in.push_str(chunk);
            if chunk.len() >= 400 {
                return;
            }
        }
        let data_b64 = std::mem::take(&mut self.sasl_in);
        let Some(mech) = self.mech.take() else { return };
        match mech {
            Mech::External => {
                // The certificate already said who we are: answer the server's "+" with an empty
                // authorisation identity ("+" again).
                self.mech = Some(Mech::External);
                self.send("AUTHENTICATE", &["+"]);
            }
            Mech::Plain => {
                // PLAIN has a single step: the server says "+", we send the credentials.
                self.mech = Some(Mech::Plain);
                let Some((acct, pass)) = self.cfg.sasl.clone() else { return };
                self.send_authenticate_payload(format!("\0{acct}\0{pass}").as_bytes());
            }
            Mech::Scram { mut state, stage } => {
                let text = match base64::engine::general_purpose::STANDARD.decode(&data_b64) {
                    Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                    Err(_) => return self.abort_sasl("the server sent data that is not valid base64".into()),
                };
                match stage {
                    ScramStage::WantGo => {
                        let first = state.client_first();
                        self.mech = Some(Mech::Scram { state, stage: ScramStage::WantFirst });
                        self.send_authenticate_payload(first.as_bytes());
                    }
                    ScramStage::WantFirst => match state.client_final(&text) {
                        Ok(fin) => {
                            self.mech = Some(Mech::Scram { state, stage: ScramStage::WantFinal });
                            self.send_authenticate_payload(fin.as_bytes());
                        }
                        Err(e) => self.abort_sasl(e),
                    },
                    ScramStage::WantFinal => match state.verify_server_final(&text) {
                        Ok(()) => {
                            self.mech = Some(Mech::Scram { state, stage: ScramStage::Verified });
                            self.send("AUTHENTICATE", &["+"]); // the empty final response
                        }
                        Err(e) => self.abort_sasl(e),
                    },
                    ScramStage::Verified => {}
                }
            }
        }
    }

    fn finish_sasl(&mut self, ok: bool, text: String) {
        self.sasl = Sasl::Done;
        self.emit(Event::SaslResult { ok, text });
        self.check_cap_end();
    }

    // ---- numerics ---------------------------------------------------------

    fn on_numeric(&mut self, m: &Message) {
        let code: u16 = m.command.parse().unwrap_or(0);
        let params: Vec<String> = m.params.iter().skip(1).cloned().collect();
        match code {
            1 => {
                if let Some(n) = m.params.first() {
                    self.nick = n.clone();
                }
                self.registered = true;
                let nick = self.nick.clone();
                self.emit(Event::Registered { nick });
            }
            5 => {
                // [me, tokens..., "are supported by this server"]
                if m.params.len() > 2 {
                    let toks = &m.params[1..m.params.len() - 1];
                    self.isupport.apply(toks);
                }
            }
            730 | 731 | 734 | 303 => self.on_presence_numeric(code, m),
            // RPL_MONLIST / RPL_ENDOFMONLIST: nothing to show
            732 | 733 => {}
            // End of the MOTD (or none): ISUPPORT is complete, send the watch list.
            376 | 422 => {
                self.watch_ready = true;
                self.sync_watch();
                self.emit(Event::Numeric { code, params });
            }
            321 => self.emit(Event::ListStart),
            322 => {
                // [me, channel, visible-users, topic]; servers may omit the topic when it is empty.
                if let [_, channel, users, rest @ ..] = m.params.as_slice() {
                    let topic = rest.first().cloned().unwrap_or_default();
                    let users = users.parse().unwrap_or(0);
                    self.emit(Event::ListEntry { channel: channel.clone(), users, topic });
                }
            }
            323 => self.emit(Event::ListEnd),
            353 => self.on_names(m),
            366 => {
                if let Some(c) = m.params.get(1).cloned() {
                    let key = self.fold(&c);
                    if let Some(ch) = self.channels.get_mut(&key) {
                        ch.names_complete = true;
                    }
                    self.touch(&key);
                    self.emit(Event::NamesDone { channel: c });
                }
            }
            332 => {
                if let [_, chan, topic, ..] = m.params.as_slice() {
                    let key = self.fold(chan);
                    if let Some(ch) = self.channels.get_mut(&key) {
                        ch.topic = Some(topic.clone());
                    }
                    self.emit(Event::Topic { channel: chan.clone(), topic: Some(topic.clone()) });
                }
            }
            // Nick problems before registration: try a variant.
            432 | 433 | 436 if !self.registered => {
                if let Some(alt) = self.cfg.alt_nicks.get(self.alt_used).cloned() {
                    self.alt_used += 1;
                    self.nick = alt.clone();
                    self.send("NICK", &[&alt]);
                } else if self.nick.len() < 30 {
                    self.nick.push('_');
                    let n = self.nick.clone();
                    self.send("NICK", &[&n]);
                } else {
                    self.emit(Event::Error { text: "no acceptable nickname".into() });
                }
                self.emit(Event::Numeric { code, params });
            }
            421 if m.params.get(1).is_some_and(|c| c.eq_ignore_ascii_case("CAP")) => {
                // Server predates CAP; carry on with plain registration.
                self.negotiating = false;
                self.send_registration();
            }
            900 => {} // RPL_LOGGEDIN; 903 follows
            903 => {
                // With SCRAM the server must have proved that it knows the password first; a 903
                // without that proof is reported as a failure even though the server says yes.
                match self.mech.take() {
                    Some(Mech::Scram { stage, .. }) if stage != ScramStage::Verified => {
                        self.finish_sasl(false, "the server accepted the login without proving its identity".into())
                    }
                    Some(Mech::Scram { .. }) => self.finish_sasl(true, "authenticated (SCRAM-SHA-256)".into()),
                    Some(Mech::External) => self.finish_sasl(true, "authenticated (EXTERNAL, client certificate)".into()),
                    _ => self.finish_sasl(true, "authenticated".into()),
                }
            }
            902 | 904 | 905 | 906 | 907 => {
                let text = m.params.last().cloned().unwrap_or_default();
                if self.sasl == Sasl::InProgress || code == 907 {
                    self.finish_sasl(code == 907, text);
                }
            }
            _ => self.emit(Event::Numeric { code, params }),
        }
    }

    fn on_names(&mut self, m: &Message) {
        let [_, _, chan, names] = m.params.as_slice() else { return };
        let key = self.fold(chan);
        let symbols: String = self.isupport.prefix.iter().map(|(_, s)| *s).collect();
        let mut members = Vec::new();
        for entry in names.split(' ').filter(|s| !s.is_empty()) {
            let nick_start = entry.find(|c: char| !symbols.contains(c)).unwrap_or(entry.len());
            let (pfx, rest) = entry.split_at(nick_start);
            let nick = rest.split('!').next().unwrap_or(rest);
            if nick.is_empty() {
                continue;
            }
            let modes: String =
                pfx.chars().filter_map(|c| self.isupport.prefix_mode_for_symbol(c)).collect();
            members.push(Member { nick: nick.to_owned(), modes });
        }
        let ch = self.channels.entry(key).or_insert_with(|| Channel::new(chan));
        for mem in members {
            let k = self.isupport.casemapping.fold(&mem.nick);
            ch.members.insert(k, mem);
        }
    }

    // ---- channel traffic --------------------------------------------------

    fn on_chat(&mut self, m: &Message) {
        let (Some(src), [target, text, ..]) = (&m.source, m.params.as_slice()) else { return };
        let is_notice = m.command == "NOTICE";
        let time = m.tag("time").map(str::to_owned);
        let msgid = m.tag("msgid").map(str::to_owned);
        let reply_to = m.tag("+draft/reply").or_else(|| m.tag("+reply")).filter(|v| !v.is_empty()).map(str::to_owned);
        let ctcp = Ctcp::parse(text);
        // Drop ignored traffic before anything else, so there is no event and no automatic reply.
        let kind = match &ctcp {
            Some(c) if is_notice || c.command != "ACTION" => IgnoreKind::Ctcp,
            _ if is_notice => IgnoreKind::Notice,
            _ => IgnoreKind::Message,
        };
        if self.is_ignored(src, target, kind) {
            return;
        }
        // A line that belongs to a history batch is collected, not shown as live traffic.
        if let Some(r) = m.tag("batch") {
            if self.batches.contains_key(r) {
                return self.collect_history(r, src, text, is_notice, ctcp, time, msgid);
            }
        }
        if let Some(c) = ctcp {
            if c.command == "ACTION" && !is_notice {
                self.emit(Event::Chat {
                    source: src.clone(),
                    target: target.clone(),
                    text: c.args,
                    kind: ChatKind::Action,
                    time,
                    msgid,
                    reply_to,
                });
                return;
            }
            if !is_notice && !self.is_me(&src.name) {
                self.auto_ctcp_reply(&src.name, &c);
            }
            self.emit(Event::Ctcp { source: src.clone(), target: target.clone(), ctcp: c, reply: is_notice });
            return;
        }
        let kind = if is_notice { ChatKind::Notice } else { ChatKind::Message };
        self.emit(Event::Chat { source: src.clone(), target: target.clone(), text: text.clone(), kind, time, msgid, reply_to });
    }

    /// Add one line to the history batch `reference`.
    #[allow(clippy::too_many_arguments)]
    fn collect_history(
        &mut self,
        reference: &str,
        src: &Source,
        text: &str,
        is_notice: bool,
        ctcp: Option<Ctcp>,
        time: Option<String>,
        msgid: Option<String>,
    ) {
        let (kind, body) = match ctcp {
            Some(c) if c.command == "ACTION" && !is_notice => (ChatKind::Action, c.args),
            Some(_) => return, // other CTCP is not history
            None if is_notice => (ChatKind::Notice, text.to_owned()),
            None => (ChatKind::Message, text.to_owned()),
        };
        if let Some(b) = self.batches.get_mut(reference) {
            if b.lines.len() < MAX_BATCH_LINES {
                b.lines.push(HistoryLine { time, source: src.clone(), text: body, kind, msgid });
            }
        }
    }

    /// `BATCH +ref type args...` opens a batch, `BATCH -ref` closes it. Only `chathistory` batches are
    /// collected (their lines come back as one [`Event::History`]); other kinds change nothing.
    fn on_batch(&mut self, m: &Message) {
        let Some(first) = m.params.first() else { return };
        if let Some(reference) = first.strip_prefix('+') {
            let kind = m.params.get(1).map(String::as_str).unwrap_or("");
            if (kind == "netsplit" || kind == "netjoin") && self.splits.len() < MAX_OPEN_BATCHES {
                let servers = m.params.iter().skip(2).take(2).cloned().collect();
                self.splits.insert(reference.to_owned(), Split { join: kind == "netjoin", servers, nicks: Vec::new(), channels: Vec::new() });
            }
            if kind == "labeled-response" && self.labeled.len() < MAX_OPEN_BATCHES {
                if let Some(l) = m.tag("label") {
                    self.labeled.insert(reference.to_owned(), l.to_owned());
                }
            }
            if HISTORY_CAPS.contains(&kind) && self.batches.len() < MAX_OPEN_BATCHES {
                let target = m.params.get(2).cloned().unwrap_or_default();
                self.batches.insert(reference.to_owned(), Batch { target, lines: Vec::new() });
            }
        } else if let Some(reference) = first.strip_prefix('-') {
            self.labeled.remove(reference);
            if let Some(b) = self.batches.remove(reference) {
                self.emit(Event::History { target: b.target, lines: b.lines });
            }
            if let Some(sp) = self.splits.remove(reference) {
                let ev = if sp.join { Event::NetJoin { servers: sp.servers, nicks: sp.nicks, channels: sp.channels } } else { Event::NetSplit { servers: sp.servers, nicks: sp.nicks, channels: sp.channels } };
                self.emit(ev);
            }
        }
    }

    /// Ask the server for the latest lines of a channel we just joined (if it has `chathistory`).
    fn request_history(&mut self, channel: &str) {
        let n = self.cfg.history_on_join;
        if n == 0 || !HISTORY_CAPS.iter().any(|c| self.enabled.contains(*c)) {
            return;
        }
        let server_max = self.isupport.raw.get("CHATHISTORY").and_then(|v| v.parse::<u32>().ok()).filter(|v| *v > 0);
        let limit = server_max.map_or(n, |max| n.min(max)).to_string();
        self.send("CHATHISTORY", &["LATEST", channel, "*", &limit]);
    }

    fn auto_ctcp_reply(&mut self, to: &str, c: &Ctcp) {
        let reply = match c.command.as_str() {
            "VERSION" => Some(Ctcp { command: "VERSION".into(), args: self.cfg.version_reply.clone() }),
            "PING" => Some(Ctcp { command: "PING".into(), args: c.args.clone() }),
            "TIME" => Some(Ctcp { command: "TIME".into(), args: (self.cfg.time_text)() }),
            _ => None,
        };
        if let Some(r) = reply {
            if self.reply_allowed() {
                self.send("NOTICE", &[to, &r.encode()]);
            }
        }
    }

    /// Rate limit for automatic CTCP replies: at most `limit` per `window`, the rest are dropped.
    /// (The request is still reported to the UI; it just does not get an answer on the wire.)
    fn reply_allowed(&mut self) -> bool {
        let (limit, window) = self.cfg.ctcp_reply_limit;
        let now = (self.cfg.monotonic)();
        while self.reply_times.front().is_some_and(|t| now.saturating_sub(*t) >= window) {
            self.reply_times.pop_front();
        }
        if self.reply_times.len() >= limit as usize {
            return false;
        }
        self.reply_times.push_back(now);
        true
    }

    /// If this line belongs to a netsplit/netjoin batch, note the nick for the batch's summary and say so.
    fn in_split(&mut self, m: &Message, nick: &str, channels: &[String]) -> bool {
        let Some(sp) = m.tag("batch").and_then(|r| self.splits.get_mut(r)) else { return false };
        for c in channels {
            if !sp.channels.iter().any(|x| x == c) && sp.channels.len() < MAX_SPLIT_NICKS {
                sp.channels.push(c.clone());
            }
        }
        if sp.nicks.len() < MAX_SPLIT_NICKS && !sp.nicks.iter().any(|n| n == nick) {
            sp.nicks.push(nick.to_owned());
        }
        true
    }

    /// A `TAGMSG` carrying `+typing` from someone else.
    fn on_tagmsg(&mut self, m: &Message) {
        let (Some(src), Some(target)) = (&m.source, m.params.first()) else { return };
        // a reaction: `+draft/react=<emoji>` (or `+draft/unreact`) with `+draft/reply=<msgid of the message>`
        let react = m.tag("+draft/react").map(|e| (e, false)).or_else(|| m.tag("+draft/unreact").map(|e| (e, true)));
        if let (Some((emoji, remove)), Some(msgid)) = (react, m.tag("+draft/reply")) {
            if !emoji.is_empty() && !msgid.is_empty() && !self.is_ignored(src, target, IgnoreKind::Message) {
                self.emit(Event::React { nick: src.name.clone(), target: target.clone(), msgid: msgid.to_owned(), emoji: emoji.to_owned(), remove });
            }
            return;
        }
        let Some(state) = m.tag("+typing").and_then(TypingState::from_tag_value) else { return };
        if self.is_me(&src.name) || self.is_ignored(src, target, IgnoreKind::Message) {
            return;
        }
        self.emit(Event::Typing { nick: src.name.clone(), target: target.clone(), state });
    }

    /// `INVITE <nick> <channel>`: someone invited us (or, with `invite-notify`, someone else to a channel we are in).
    fn on_invite(&mut self, m: &Message) {
        let (Some(src), [nick, channel, ..]) = (&m.source, m.params.as_slice()) else { return };
        if self.is_ignored(src, channel, IgnoreKind::Message) {
            return;
        }
        self.emit(Event::Invite { by: src.name.clone(), nick: nick.clone(), channel: channel.clone() });
    }

    /// Send a message with a `label`, so the server's answer can be recognised (`labeled-response`). Without that
    /// capability the message goes out as it is.
    pub fn send_labeled(&mut self, mut m: Message, label: &str) {
        if self.enabled.contains("labeled-response") {
            m.tags.push(chimerairc_proto::Tag { key: "label".into(), value: Some(label.into()) });
        }
        self.outbox.push(m);
    }

    fn tag_allowed(&self, tag: &str) -> bool {
        self.enabled.contains("message-tags") && crate::isupport::client_tag_allowed(self.isupport.raw.get("CLIENTTAGDENY").map(String::as_str), tag)
    }

    /// Say `text` to `target` as a reply to the message `msgid`. Where the server does not carry reply tags it is
    /// said as an ordinary message. Returns whether the reply tag was used.
    pub fn send_reply(&mut self, target: &str, msgid: &str, text: &str) -> bool {
        let mut m = Message::new("PRIVMSG", &[target, text]);
        let tagged = self.tag_allowed("draft/reply") && !msgid.is_empty();
        if tagged {
            m.tags.push(chimerairc_proto::Tag { key: "+draft/reply".into(), value: Some(msgid.into()) });
        }
        self.outbox.push(m);
        tagged
    }

    /// React to the message `msgid` in `target` with `emoji` (or take the reaction back). Needs a server that
    /// carries the tags; otherwise nothing is sent and false is returned.
    pub fn send_react(&mut self, target: &str, msgid: &str, emoji: &str, remove: bool) -> bool {
        let bad = emoji.is_empty() || emoji.len() > 64 || emoji.chars().any(|c| c.is_control() || c.is_whitespace());
        if bad || msgid.is_empty() || !self.tag_allowed("draft/react") || !self.tag_allowed("draft/reply") {
            return false;
        }
        let mut m = Message::new("TAGMSG", &[target]);
        m.tags.push(chimerairc_proto::Tag { key: if remove { "+draft/unreact" } else { "+draft/react" }.into(), value: Some(emoji.into()) });
        m.tags.push(chimerairc_proto::Tag { key: "+draft/reply".into(), value: Some(msgid.into()) });
        self.outbox.push(m);
        true
    }

    /// Tell `target` (a channel or a nick) what we are doing with the entry box. Sent only when the server has
    /// `message-tags` and does not forbid the `typing` tag (`CLIENTTAGDENY`); otherwise nothing is sent.
    pub fn typing(&mut self, target: &str, state: TypingState) {
        if !self.enabled.contains("message-tags") || !crate::isupport::client_tag_allowed(self.isupport.raw.get("CLIENTTAGDENY").map(String::as_str), "typing") {
            return;
        }
        let mut m = Message::new("TAGMSG", &[target]);
        m.tags.push(chimerairc_proto::Tag { key: "+typing".into(), value: Some(state.tag_value().into()) });
        self.outbox.push(m);
    }

    fn on_join(&mut self, m: &Message) {
        let (Some(src), Some(chan)) = (&m.source, m.params.first()) else { return };
        let account = m.params.get(1).filter(|a| *a != "*").cloned();
        let realname = m.params.get(2).cloned();
        let key = self.fold(chan);
        if self.is_me(&src.name) {
            self.channels.insert(key.clone(), Channel::new(chan));
            self.request_history(chan);
        }
        let nk = self.fold(&src.name);
        if let Some(ch) = self.channels.get_mut(&key) {
            ch.members.insert(nk, Member { nick: src.name.clone(), modes: String::new() });
        }
        self.touch(&key);
        if self.in_split(m, &src.name, std::slice::from_ref(chan)) {
            return;
        }
        self.emit(Event::Join { channel: chan.clone(), nick: src.name.clone(), account, realname });
    }

    fn on_part(&mut self, m: &Message) {
        let (Some(src), Some(chan)) = (&m.source, m.params.first()) else { return };
        let key = self.fold(chan);
        if self.is_me(&src.name) {
            self.channels.remove(&key);
        } else {
            let nk = self.fold(&src.name);
            if let Some(ch) = self.channels.get_mut(&key) {
                ch.members.remove(&nk);
            }
            self.touch(&key);
        }
        self.emit(Event::Part { channel: chan.clone(), nick: src.name.clone(), reason: m.params.get(1).cloned() });
    }

    fn on_quit(&mut self, m: &Message) {
        let Some(src) = &m.source else { return };
        let nk = self.fold(&src.name);
        let mut chans = Vec::new();
        for (k, ch) in self.channels.iter_mut() {
            if ch.members.remove(&nk).is_some() {
                chans.push(ch.name.clone());
                if ch.names_complete {
                    self.dirty.insert(k.clone());
                }
            }
        }
        if self.in_split(m, &src.name, &chans) {
            return;
        }
        self.emit(Event::Quit { nick: src.name.clone(), reason: m.params.first().cloned(), channels: chans });
    }

    fn on_kick(&mut self, m: &Message) {
        let (Some(src), [chan, victim, ..]) = (&m.source, m.params.as_slice()) else { return };
        let key = self.fold(chan);
        if self.is_me(victim) {
            self.channels.remove(&key);
        } else {
            let vk = self.fold(victim);
            if let Some(ch) = self.channels.get_mut(&key) {
                ch.members.remove(&vk);
            }
            self.touch(&key);
        }
        self.emit(Event::Kick {
            channel: chan.clone(),
            by: src.name.clone(),
            victim: victim.clone(),
            reason: m.params.get(2).cloned(),
        });
    }

    fn on_nick(&mut self, m: &Message) {
        let (Some(src), Some(new)) = (&m.source, m.params.first()) else { return };
        let (ok, nk) = (self.fold(&src.name), self.fold(new));
        if self.is_me(&src.name) {
            self.nick = new.clone();
        }
        for (k, ch) in self.channels.iter_mut() {
            if let Some(mut mem) = ch.members.remove(&ok) {
                mem.nick = new.clone();
                ch.members.insert(nk.clone(), mem);
                if ch.names_complete {
                    self.dirty.insert(k.clone());
                }
            }
        }
        self.emit(Event::Nick { old: src.name.clone(), new: new.clone() });
    }

    fn on_topic(&mut self, m: &Message) {
        let Some(chan) = m.params.first() else { return };
        let topic = m.params.get(1).filter(|t| !t.is_empty()).cloned();
        let key = self.fold(chan);
        if let Some(ch) = self.channels.get_mut(&key) {
            ch.topic = topic.clone();
        }
        self.emit(Event::Topic { channel: chan.clone(), topic });
    }

    fn on_mode(&mut self, m: &Message) {
        let [target, modes, args @ ..] = m.params.as_slice() else { return };
        if !self.isupport.is_channel(target) {
            return; // user modes: not tracked yet
        }
        let changes = self.parse_modes(modes, args);
        let key = self.fold(target);
        for c in &changes {
            if let Some((_, _)) = self.isupport.prefix.iter().find(|(l, _)| *l == c.mode) {
                if let Some(arg) = &c.arg {
                    let nk = self.fold(arg);
                    let rank: Vec<char> = self.isupport.prefix.iter().map(|(l, _)| *l).collect();
                    if let Some(mem) = self.channels.get_mut(&key).and_then(|ch| ch.members.get_mut(&nk)) {
                        let mut set: Vec<char> = mem.modes.chars().filter(|x| *x != c.mode).collect();
                        if c.adding {
                            set.push(c.mode);
                        }
                        set.sort_by_key(|x| rank.iter().position(|r| r == x));
                        mem.modes = set.into_iter().collect();
                        self.dirty.insert(key.clone());
                    }
                }
            } else if !self.isupport.chanmodes[0].contains(c.mode) {
                if let Some(ch) = self.channels.get_mut(&key) {
                    ch.modes.retain(|x| x != c.mode);
                    if c.adding {
                        ch.modes.push(c.mode);
                    }
                }
            }
        }
        let by = m.source.as_ref().map(|s: &Source| s.name.clone());
        self.emit(Event::Mode { target: target.clone(), by, changes });
    }

    /// Split a mode string into changes, consuming arguments per CHANMODES/PREFIX.
    pub fn parse_modes(&self, modes: &str, args: &[String]) -> Vec<ModeChange> {
        let cm = &self.isupport.chanmodes;
        let mut args = args.iter();
        let mut adding = true;
        let mut out = Vec::new();
        for c in modes.chars() {
            match c {
                '+' => adding = true,
                '-' => adding = false,
                c => {
                    let is_prefix = self.isupport.prefix.iter().any(|(l, _)| *l == c);
                    let takes_arg = is_prefix
                        || cm[0].contains(c)
                        || cm[1].contains(c)
                        || (cm[2].contains(c) && adding);
                    let arg = if takes_arg { args.next().cloned() } else { None };
                    out.push(ModeChange { adding, mode: c, arg });
                }
            }
        }
        out
    }
}
