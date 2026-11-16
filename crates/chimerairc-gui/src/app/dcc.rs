//! DCC in the application: incoming offers (always confirmed by the user), outgoing files and
//! chats, and the book-keeping behind the Transfers window.
//!
//! Security rules enforced here (the address and filename rules live in `chimerairc_proto::dcc`, the
//! transport guarantees in `chimerairc_core::dcc`):
//! * nothing is ever accepted automatically: every offer is a prompt that defaults to No;
//! * the user always picks where a received file is saved (Save As, with an overwrite prompt);
//! * offers that aim us at a forbidden address are refused before the user is even asked;
//! * only one prompt is shown at a time, and a flood of offers is capped and dropped.

use super::*;
use std::collections::BTreeMap;
use std::future::Future;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::task::AbortHandle;
use chimerairc_core::dcc::{self as xport, ChatHandle, DccEvent};
use chimerairc_core::portmap;
use chimerairc_proto::dcc as proto;
use chimerairc_proto::{Ctcp, Source};

/// Offers waiting for their turn at the prompt; beyond this, new ones are dropped.
const MAX_PENDING: usize = 6;
/// Transfers/chats that may be waiting or running at once.
const MAX_ACTIVE: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum XKind {
    Send,
    Receive,
    Chat,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Status {
    Waiting,
    Active,
    Done,
    Failed(String),
    Cancelled,
}

impl Status {
    fn running(&self) -> bool {
        matches!(self, Status::Waiting | Status::Active)
    }
}

pub struct Xfer {
    id: u32,
    conn: u32,
    kind: XKind,
    peer: String,
    name: String,
    path: Option<PathBuf>,
    total: u64,
    bytes: u64,
    started: Instant,
    status: Status,
    abort: Option<AbortHandle>,
    chat: Option<ChatHandle>,
    chat_win: isize,
    /// Bytes that were already there when this transfer started (a resumed download or upload);
    /// excluded from the speed figure.
    base: u64,
    /// Sending: the port we listen on, and where a peer's resume position goes.
    listen_port: u16,
    resume_pos: Option<Arc<AtomicU64>>,
    /// Receiving: a RESUME request is out and we are waiting for the sender's ACCEPT.
    resume_wait: Option<ResumeWait>,
    /// Passive (reverse) offer we made: the token the peer must echo, and (for chats) the receiving
    /// end of the line channel until the peer tells us where to connect.
    passive_token: Option<String>,
    /// A UPnP port mapping to remove once the peer has connected (or the offer is over).
    mapping: Option<portmap::Mapping>,
    /// Turbo mode (DCC TSEND): no acknowledgements. Used when we send.
    turbo: bool,
    /// Secure DCC (SSEND/SCHAT): the connection runs inside TLS. Used when we offer.
    secure: bool,
    /// The connection is encrypted (the TLS handshake has completed).
    encrypted: bool,
    chat_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
}

/// What is needed to start the download once the sender answers a resume request.
struct ResumeWait {
    ip: IpAddr,
    /// 0 for a passive offer (then `token` is the offer's token and `ip` is unused).
    port: u16,
    size: u64,
    have: u64,
    server: Option<IpAddr>,
    filename: String,
    token: Option<String>,
    turbo: bool,
    secure: bool,
}

/// How long to wait for the sender to answer a RESUME request.
const RESUME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

struct Offer {
    conn: u32,
    nick: String,
    mask: String,
    req: proto::Dcc,
}

/// After this many declined offers from one nick in a session, its further offers are dropped
/// silently (a spammer must not be able to make you click "No" forever).
const DECLINE_LIMIT: u32 = 3;

/// Counts declined offers per nick (case-insensitive).
#[derive(Default)]
pub struct Declines(std::collections::HashMap<String, u32>);

impl Declines {
    /// Record a decline. True when this one reaches the limit (so the caller can say so once).
    fn record(&mut self, nick: &str) -> bool {
        let n = self.0.entry(nick.to_lowercase()).or_insert(0);
        *n += 1;
        *n == DECLINE_LIMIT
    }
    fn blocked(&self, nick: &str) -> bool {
        self.0.get(&nick.to_lowercase()).is_some_and(|n| *n >= DECLINE_LIMIT)
    }
    fn allow(&mut self, nick: &str) -> bool {
        self.0.remove(&nick.to_lowercase()).is_some()
    }
}

#[derive(Default)]
pub struct State {
    xfers: BTreeMap<u32, Xfer>,
    next: u32,
    pending: VecDeque<Offer>,
    prompting: bool,
    declines: Declines,
}

/// The user said No (or chose nothing): count it, and warn once when the nick hits the limit.
fn declined(conn: u32, nick: &str) {
    if with(|a| a.dcc.declines.record(nick)) {
        server_print(
            conn,
            &format!("* Declined {DECLINE_LIMIT} DCC offers from {nick}: further offers from them are ignored this session (/dcc allow {nick} to undo)."),
        );
    }
}

static DCC_QUEUE: Mutex<VecDeque<(u32, DccEvent)>> = Mutex::new(VecDeque::new());

/// An event sink for a transfer task: queue it and wake the UI thread.
fn emitter(id: u32) -> impl Fn(DccEvent) + Send + Sync + 'static {
    move |e| {
        DCC_QUEUE.lock().unwrap().push_back((id, e));
        super::wake_ui();
    }
}

fn spawn_for<F: Future<Output = ()> + Send + 'static>(id: u32, fut: F) {
    let handle = with(|a| a.rt.spawn(fut));
    with(|a| {
        if let Some(x) = a.dcc.xfers.get_mut(&id) {
            x.abort = Some(handle.abort_handle());
        }
    });
}

fn new_xfer(conn: u32, kind: XKind, peer: &str, name: &str, path: Option<PathBuf>, total: u64) -> u32 {
    with(|a| {
        a.dcc.next += 1;
        let id = a.dcc.next;
        a.dcc.xfers.insert(
            id,
            Xfer {
                id,
                conn,
                kind,
                peer: peer.to_owned(),
                name: name.to_owned(),
                path,
                total,
                bytes: 0,
                started: Instant::now(),
                status: Status::Waiting,
                abort: None,
                chat: None,
                chat_win: 0,
                base: 0,
                listen_port: 0,
                resume_pos: None,
                resume_wait: None,
                passive_token: None,
                mapping: None,
                turbo: false,
                secure: false,
                encrypted: false,
                chat_rx: None,
            },
        );
        id
    })
}

fn too_busy() -> bool {
    with(|a| a.dcc.xfers.values().filter(|x| x.status.running()).count() >= MAX_ACTIVE)
}

fn registered_client(h: HWND, conn: u32) -> Option<Client> {
    let c = with(|a| a.conns.get(&conn).filter(|c| c.registered).and_then(|c| c.client.clone()));
    if c.is_none() {
        local(h, "* Connect to a server first.");
    }
    c
}

/// The address to put in an offer: the one set in Preferences, else our end of the IRC connection.
fn advertise_ip(client: &Client) -> Result<IpAddr, String> {
    let configured = settings().dcc_ip;
    if !configured.trim().is_empty() {
        return configured
            .trim()
            .parse::<IpAddr>()
            .map_err(|_| "the DCC address set in Preferences is not a valid IP address".to_owned());
    }
    match client.conn_info().local {
        // an IPv4 address that arrived wrapped in IPv6 is still an IPv4 address
        Some(IpAddr::V6(v6)) if v6.to_ipv4_mapped().is_some() => Ok(IpAddr::V4(v6.to_ipv4_mapped().unwrap())),
        Some(ip) if !ip.is_unspecified() => Ok(ip),
        _ => Err("could not work out your address; set it in Preferences".into()),
    }
}

/// Listen on the same address family as the `advertised` address that will go into the offer.
fn listen_port(advertised: IpAddr) -> Result<(tokio::net::TcpListener, u16), String> {
    let ports = parse_port_range(&settings().dcc_ports);
    with(|a| a.rt.block_on(xport::listen_for(advertised, ports)))
        .map_err(|e| format!("could not open a port to listen on: {e}"))
}

/// What the accept prompt says about a secure (TLS) offer.
const SECURE_NOTE: &str = "Security:  encrypted with TLS (secure DCC). Nobody listening in can read it, but \
                           the other side is not verified: you cannot tell it really is who it says it is.\n";

/// A token to pair a passive offer with its answer.
fn new_token() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{}", 100_000_000 + (d.as_nanos() % 900_000_000) as u64)
}

/// True when the peer cannot reach the address we would advertise (we are behind a router and no
/// public address is configured), so a passive offer is the only kind that can work.
fn behind_nat(ip: IpAddr, server: Option<IpAddr>) -> bool {
    nat_rule(settings().dcc_ip.trim().is_empty(), ip, server)
}

/// The rule itself: no public address configured, our address is private, and the server is on the
/// internet (a server on the same LAN can reach us directly).
fn nat_rule(no_configured_ip: bool, ip: IpAddr, server: Option<IpAddr>) -> bool {
    no_configured_ip && !proto::is_public(ip) && server.is_none_or(proto::is_public)
}

/// After the timeout, give up on a passive offer nobody answered.
fn watch_reply(id: u32, chat: bool) {
    let e = emitter(id);
    spawn_for(id, async move {
        tokio::time::sleep(xport::ACCEPT_TIMEOUT).await;
        let reason = "no reply to the passive offer (the other side may not support reverse DCC)".to_owned();
        e(if chat { DccEvent::ChatClosed { reason } } else { DccEvent::Failed { reason } });
    });
}

/// A listening socket together with the address to put in the offer for it.
struct Listening {
    listener: tokio::net::TcpListener,
    port: u16,
    ip: IpAddr,
    mapping: Option<portmap::Mapping>,
}

fn listen_plain(ip: IpAddr) -> Result<Listening, String> {
    let (listener, port) = listen_port(ip)?;
    Ok(Listening { listener, port, ip, mapping: None })
}

/// How to look for the router: PCP and NAT-PMP at the default gateway, UPnP by multicast search.
/// The advanced environment variables `CHIMERAIRC_PMP_ADDR` (ip:port of the gateway) and
/// `CHIMERAIRC_UPNP_SEARCH_ADDR` (ip:port to send the UPnP search to) override those, which is how
/// the feature is tested against a fake router.
fn portmap_options() -> portmap::Options {
    let env_addr = |name: &str| std::env::var(name).ok().and_then(|s| s.parse::<std::net::SocketAddr>().ok());
    let mut o = portmap::Options::new(crate::util::default_gateway());
    if let Some(a) = env_addr("CHIMERAIRC_PMP_ADDR") {
        o.gateway = Some(a);
    }
    if let Some(a) = env_addr("CHIMERAIRC_UPNP_SEARCH_ADDR") {
        o.ssdp = a;
    }
    o
}

/// Port forwarding only helps when it is switched on, we are on IPv4 and behind a router.
fn upnp_wanted(ip: IpAddr, server: Option<IpAddr>) -> bool {
    upnp_rule(settings().dcc_upnp, settings().dcc_ip.trim().is_empty(), ip, server)
}

fn upnp_rule(enabled: bool, no_configured_ip: bool, ip: IpAddr, server: Option<IpAddr>) -> bool {
    enabled && ip.is_ipv4() && nat_rule(no_configured_ip, ip, server)
}

/// Open a port and ask the router to forward it (PCP, NAT-PMP or UPnP, whichever it speaks). The
/// offer then carries the router's address. (Blocks the UI for as long as the router takes, a few
/// seconds at most, longer only when the router speaks none of them.)
fn listen_upnp(ip: IpAddr) -> Result<Listening, String> {
    let (listener, port) = listen_port(ip)?;
    let options = portmap_options();
    let m = with(|a| a.rt.block_on(portmap::open(&options, port, 300)))?;
    Ok(Listening { listener, port, ip: IpAddr::V4(m.external_ip), mapping: Some(m) })
}

/// Listen for a connection to an offer we are about to make or answer: through a forwarded router
/// port when that is wanted and works, otherwise on our own address (saying why not).
fn listen_for_offer(ip: IpAddr, server: Option<IpAddr>, say: &dyn Fn(&str)) -> Result<Listening, String> {
    if upnp_wanted(ip, server) {
        match listen_upnp(ip) {
            Ok(l) => return Ok(l),
            Err(e) => say(&format!("* Port forwarding: {e}; the offer uses your own address, which may not be reachable.")),
        }
    }
    listen_plain(ip)
}

/// Remove the router mapping of a transfer once it no longer needs it (the peer connected, or the
/// transfer ended another way).
fn release_mapping(id: u32) {
    let m = with(|a| a.dcc.xfers.get_mut(&id).filter(|x| x.status != Status::Waiting).and_then(|x| x.mapping.take()));
    if let Some(m) = m {
        with(|a| {
            a.rt.spawn(async move {
                let _ = m.close().await;
            })
        });
    }
}

fn open_window() {
    super::dccwin_open();
}

// ---------------------------------------------------------------- incoming offers

/// A CTCP request `DCC ...` arrived. Returns true if it was a DCC request (handled here).
pub(super) fn on_ctcp(conn: u32, src: &Source, c: &Ctcp) -> bool {
    if c.command != "DCC" {
        return false;
    }
    if with(|a| a.dcc.declines.blocked(&src.name)) {
        return true; // they were declined too often: drop silently
    }
    match proto::parse(c) {
        // Resume handshake messages answer something we sent; they are never prompts.
        Ok(proto::Dcc::Accept { port, position, token, .. }) => on_accept(conn, &src.name, port, position, token),
        Ok(proto::Dcc::Resume { port, position, token, .. }) => on_resume(conn, &src.name, port, position, token),
        Ok(req) => {
            // The answer to a passive offer of ours is not a new offer.
            if on_passive_reply(conn, &src.name, &req) {
                return true;
            }
            let offer = Offer { conn, nick: src.name.clone(), mask: src.mask().unwrap_or_else(|| src.name.clone()), req };
            let accepted = with(|a| {
                if a.dcc.pending.len() >= MAX_PENDING {
                    false
                } else {
                    a.dcc.pending.push_back(offer);
                    true
                }
            });
            if accepted {
                process_offers();
            } else {
                server_print(conn, &format!("* Too many DCC offers waiting; dropped one from {}.", src.name));
            }
        }
        Err(proto::ParseError::Unsupported) => {
            let what = c.args.split_whitespace().next().unwrap_or("?").to_ascii_uppercase();
            server_print(conn, &format!("* {} sent an unsupported DCC request ({what}); ignored.", src.name));
        }
        Err(_) => server_print(conn, &format!("* {} sent a malformed DCC request; ignored.", src.name)),
    }
    true
}

/// The peer answered a passive offer of ours with an address, port and our token: connect to it.
/// Returns true if the message was such an answer (it is then never shown as an offer).
fn on_passive_reply(conn: u32, nick: &str, req: &proto::Dcc) -> bool {
    let (ip, port, token, is_chat) = match req {
        proto::Dcc::Send { ip, port, token: Some(t), .. } if *port != 0 => (*ip, *port, t.clone(), false),
        proto::Dcc::Chat { ip, port, token: Some(t), .. } if *port != 0 => (*ip, *port, t.clone(), true),
        _ => return false,
    };
    let found = with(|a| {
        let x = a.dcc.xfers.values_mut().find(|x| {
            x.conn == conn
                && x.status == Status::Waiting
                && eq_ic(&x.peer, nick)
                && (x.kind == XKind::Chat) == is_chat
                && x.passive_token.as_deref() == Some(token.as_str())
        })?;
        if let Some(ab) = x.abort.take() {
            ab.abort(); // the no-reply watcher
        }
        x.passive_token = None;
        x.started = Instant::now();
        Some((x.id, x.path.clone(), x.chat_rx.take(), x.resume_pos.clone(), x.turbo, x.secure))
    });
    let Some((id, path, rx, resume, turbo, secure)) = found else { return false };
    let server = with(|a| a.conns.get(&conn).and_then(|c| c.client.as_ref().map(|c| c.conn_info().server))).flatten();
    if let Err(why) = proto::target_allowed(ip, port, server) {
        fail_now(id, conn, &format!("refused the address {nick} gave for the passive offer: {why}"));
        super::dccwin_refresh();
        return true;
    }
    if is_chat {
        if let Some(rx) = rx {
            spawn_for(id, xport::chat_connect(ip, port, server, rx, secure, emitter(id)));
        }
    } else if let Some(path) = path {
        let resume = resume.unwrap_or_else(|| Arc::new(AtomicU64::new(0)));
        spawn_for(id, xport::send_file_to(path, ip, port, server, resume, turbo, secure, emitter(id)));
    }
    super::dccwin_refresh();
    true
}

/// The sender agreed to our RESUME request: start the download from the part we have.
fn on_accept(conn: u32, nick: &str, port: u16, position: u64, token: Option<String>) {
    let started = with(|a| {
        let x = a.dcc.xfers.values_mut().find(|x| {
            x.kind == XKind::Receive
                && x.conn == conn
                && x.status == Status::Waiting
                && eq_ic(&x.peer, nick)
                && x.resume_wait.as_ref().is_some_and(|w| w.port == port && w.have == position && w.token == token)
        })?;
        if let Some(ab) = x.abort.take() {
            ab.abort(); // the timeout watcher
        }
        x.started = Instant::now();
        Some((x.id, x.path.clone()?, x.resume_wait.take()?))
    });
    // An ACCEPT we did not ask for (or a late/duplicate one) is ignored.
    let Some((id, path, w)) = started else { return };
    if w.port == 0 {
        // Passive offer: now we listen and tell them where to connect.
        start_passive_receive(id, conn, nick, &w.filename, w.size, path, w.have, w.turbo, w.secure, w.token.as_deref());
    } else {
        spawn_for(id, xport::receive_file(w.ip, w.port, w.size, path, w.have, w.server, w.turbo, w.secure, emitter(id)));
    }
    super::dccwin_refresh();
}

/// Reverse DCC download: listen, answer the passive offer with our address and port (and their
/// token), and receive from whoever connects.
#[allow(clippy::too_many_arguments)]
fn start_passive_receive(
    id: u32,
    conn: u32,
    nick: &str,
    filename: &str,
    size: u64,
    path: PathBuf,
    resume_from: u64,
    turbo: bool,
    secure: bool,
    token: Option<&str>,
) {
    let Some(client) = with(|a| a.conns.get(&conn).and_then(|c| c.client.clone())) else {
        return fail_now(id, conn, "the connection to the server was lost");
    };
    let ip = match advertise_ip(&client) {
        Ok(ip) => ip,
        Err(e) => return fail_now(id, conn, &e),
    };
    let server = client.conn_info().server;
    let l = match listen_for_offer(ip, server, &|m| server_print(conn, m)) {
        Ok(l) => l,
        Err(e) => return fail_now(id, conn, &e),
    };
    let reply = proto::send_offer_kind(turbo, secure, filename, l.ip, l.port, size, token);
    send(conn, Message::new("PRIVMSG", &[nick, &reply.encode()]));
    with(|a| {
        if let Some(x) = a.dcc.xfers.get_mut(&id) {
            x.mapping = l.mapping.clone();
        }
    });
    spawn_for(id, xport::receive_file_passive(l.listener, xport::ACCEPT_TIMEOUT, size, path, resume_from, turbo, secure, emitter(id)));
}

/// A peer wants to resume a file we offered: if it matches an offer still waiting for a
/// connection, set the start position and agree.
fn on_resume(conn: u32, nick: &str, port: u16, position: u64, token: Option<String>) {
    let found = with(|a| {
        let x = a.dcc.xfers.values_mut().find(|x| {
            x.kind == XKind::Send
                && x.conn == conn
                && x.status == Status::Waiting
                && eq_ic(&x.peer, nick)
                && x.resume_pos.is_some()
                // our listening port, or (passive offer) port 0 plus the token of that offer
                && if port != 0 {
                    x.listen_port == port && x.passive_token.is_none()
                } else {
                    token.is_some() && x.passive_token == token
                }
        })?;
        if position == 0 || position >= x.total {
            return None;
        }
        x.resume_pos.as_ref()?.store(position, Ordering::SeqCst);
        x.base = position;
        x.bytes = position;
        Some(x.name.clone())
    });
    let Some(name) = found else { return };
    let reply = proto::accept_reply(&name, port, position, token.as_deref());
    send(conn, Message::new("PRIVMSG", &[nick, &reply.encode()]));
    server_print(conn, &format!("* Resuming {name} to {nick} from {}.", human_size(position)));
    super::dccwin_refresh();
}

/// Ask the sender to continue from `have` bytes; the download starts when it answers.
fn start_resume(id: u32, conn: u32, nick: &str, w: ResumeWait) {
    let reply = proto::resume_request(&w.filename, w.port, w.have, w.token.as_deref());
    send(conn, Message::new("PRIVMSG", &[nick, &reply.encode()]));
    with(|a| {
        if let Some(x) = a.dcc.xfers.get_mut(&id) {
            x.base = w.have;
            x.bytes = w.have;
            x.resume_wait = Some(w);
        }
    });
    let e = emitter(id);
    spawn_for(id, async move {
        tokio::time::sleep(RESUME_TIMEOUT).await;
        e(DccEvent::Failed { reason: "the sender did not answer the resume request; the partial file is kept".into() });
    });
}

/// Show the queued offers one at a time. The prompt is modal and runs a nested message loop, so
/// offers that arrive meanwhile are only queued; the flag stops recursive prompting.
fn process_offers() {
    if with(|a| std::mem::replace(&mut a.dcc.prompting, true)) {
        return;
    }
    while let Some(o) = with(|a| a.dcc.pending.pop_front()) {
        handle_offer(o);
    }
    with(|a| a.dcc.prompting = false);
}

fn handle_offer(o: Offer) {
    let owner = ui::frame();
    let Some(client) = with(|a| a.conns.get(&o.conn).and_then(|c| c.client.clone())) else { return };
    let info = client.conn_info();
    if too_busy() {
        server_print(o.conn, &format!("* Too many DCC transfers running; declined the offer from {}.", o.nick));
        log_offer(&o, "Refused", "", 0, "too many transfers running");
        return;
    }
    let rules = with(|a| dccrules::Rules::new(&a.settings.dcc_reject_exts, a.settings.dcc_max_mb, a.settings.dcc_reject_chat));
    match o.req.clone() {
        // Handled directly in `on_ctcp`; never queued as an offer.
        proto::Dcc::Resume { .. } | proto::Dcc::Accept { .. } => {}
        proto::Dcc::Send { filename, ip, port, size, token, turbo, secure } => {
            let passive = o.req.is_passive();
            if !passive {
                if let Err(why) = proto::target_allowed(ip, port, info.server) {
                    server_print(o.conn, &format!("* Refused a DCC file offer from {}: {why}.", o.nick));
                    log_offer(&o, "Refused", &filename, size, why);
                    return;
                }
            }
            let safe = proto::safe_filename(&filename);
            if let Some(why) = rules.file(&filename, &safe, size) {
                server_print(o.conn, &format!("* Auto-rejected the file {safe} from {}: {why}.", o.nick));
                log_offer(&o, "Rejected", &safe, size, &why);
                return;
            }
            let from = if passive {
                "Connection:  they ask you to listen (passive DCC)".to_owned()
            } else {
                format!("From:  {ip}:{port}")
            };
            let mut text = format!(
                "{} ({}) wants to send you a file.\n\nFile:  {safe}\nSize:  {}\n{from}\n",
                o.nick,
                o.mask,
                human_size(size)
            );
            if turbo {
                text.push_str("Mode:  turbo (TSEND/XMIT: no acknowledgements are exchanged)\n");
            }
            if secure {
                text.push_str(SECURE_NOTE);
            }
            if safe != filename {
                let shown: String = filename.chars().take(80).collect();
                text.push_str(&format!("\nThey named it \"{shown}\"; it will be saved as \"{safe}\".\n"));
            }
            if proto::is_risky_extension(&safe) {
                text.push_str(&format!(
                    "\nWARNING: this is a program or script type. Opening it can harm your computer. \
                     Only accept it if you know and trust {}.\n",
                    o.nick
                ));
            }
            text.push_str("\nAccept this file?");
            if !dialogs::confirm(owner, "Incoming DCC file", &text) {
                server_print(o.conn, &format!("* Declined the file {safe} from {}.", o.nick));
                log_offer(&o, "Declined", &safe, size, "");
                declined(o.conn, &o.nick);
                return;
            }
            let Some(path) = dialogs::pick_save_file(owner, &safe, "Save the DCC file as") else {
                server_print(o.conn, &format!("* Declined the file {safe} from {} (no save location chosen).", o.nick));
                log_offer(&o, "Declined", &safe, size, "no save location chosen");
                declined(o.conn, &o.nick);
                return;
            };
            // A partial download from an earlier attempt: offer to continue it.
            let have = std::fs::metadata(xport::part_path(&path)).map(|m| m.len()).unwrap_or(0);
            let mut resume_have = 0;
            if have > 0 && have < size {
                let q = format!(
                    "A partial download of {safe} is already on disk ({} of {}).\n\n\
                     Yes: continue where it stopped\nNo: start over\nCancel: don't download",
                    human_size(have),
                    human_size(size)
                );
                match dialogs::ask3(owner, "Resume DCC download", &q) {
                    None => {
                        server_print(o.conn, &format!("* Declined the file {safe} from {}.", o.nick));
                        log_offer(&o, "Declined", &safe, size, "cancelled at the resume question");
                        declined(o.conn, &o.nick);
                        return;
                    }
                    Some(true) => resume_have = have,
                    Some(false) => {}
                }
            }
            let id = new_xfer(o.conn, XKind::Receive, &o.nick, &safe, Some(path.clone()), size);
            if passive {
                if resume_have > 0 {
                    // Ask first; when they agree we listen and answer (see `on_accept`).
                    let w = ResumeWait {
                        ip,
                        port: 0,
                        size,
                        have: resume_have,
                        server: info.server,
                        filename: filename.clone(),
                        token: token.clone(),
                        turbo,
                        secure,
                    };
                    start_resume(id, o.conn, &o.nick, w);
                } else {
                    start_passive_receive(id, o.conn, &o.nick, &filename, size, path, 0, turbo, secure, token.as_deref());
                }
            } else {
                if resume_have > 0 {
                    let w = ResumeWait {
                        ip,
                        port,
                        size,
                        have: resume_have,
                        server: info.server,
                        filename: filename.clone(),
                        token: None,
                        turbo,
                        secure,
                    };
                    start_resume(id, o.conn, &o.nick, w);
                } else {
                    spawn_for(id, xport::receive_file(ip, port, size, path, 0, info.server, turbo, secure, emitter(id)));
                }
            }
            open_window();
        }
        proto::Dcc::Chat { ip, port, token, secure } => {
            let passive = o.req.is_passive();
            if let Some(why) = rules.chat() {
                server_print(o.conn, &format!("* Auto-rejected a DCC chat request from {}: {why}.", o.nick));
                log_offer(&o, "Rejected", "", 0, why);
                return;
            }
            if !passive {
                if let Err(why) = proto::target_allowed(ip, port, info.server) {
                    server_print(o.conn, &format!("* Refused a DCC chat offer from {}: {why}.", o.nick));
                    log_offer(&o, "Refused", "", 0, why);
                    return;
                }
            }
            let how = if passive {
                "They can't be reached directly, so they ask you to listen: they will connect to your computer, \
                 bypassing the IRC server."
                    .to_owned()
            } else {
                format!("This makes a direct connection from your computer to {ip}:{port}, bypassing the IRC server.")
            };
            let note = if secure { SECURE_NOTE } else { "" };
            let text = format!("{} ({}) wants to start a DCC chat.\n\n{how}\n{note}\nStart the chat?", o.nick, o.mask);
            if !dialogs::confirm(owner, "Incoming DCC chat", &text) {
                server_print(o.conn, &format!("* Declined the DCC chat with {}.", o.nick));
                log_offer(&o, "Declined", "", 0, "");
                declined(o.conn, &o.nick);
                return;
            }
            let id = new_xfer(o.conn, XKind::Chat, &o.nick, &format!("chat with {}", o.nick), None, 0);
            let win = chat_window(o.conn, id, &o.nick);
            let (handle, rx) = xport::chat_channel();
            with(|a| {
                if let Some(x) = a.dcc.xfers.get_mut(&id) {
                    x.chat = Some(handle);
                }
            });
            if passive {
                let setup = advertise_ip(&client)
                    .and_then(|my_ip| listen_for_offer(my_ip, info.server, &|m| super::print(win, m)));
                match setup {
                    Ok(l) => {
                        let reply = proto::chat_offer_token_kind(secure, l.ip, l.port, token.as_deref().unwrap_or(""));
                        send(o.conn, Message::new("PRIVMSG", &[&o.nick, &reply.encode()]));
                        super::print(win, &format!("* Waiting for {} to connect (port {})...", o.nick, l.port));
                        with(|a| {
                            if let Some(x) = a.dcc.xfers.get_mut(&id) {
                                x.mapping = l.mapping.clone();
                            }
                        });
                        spawn_for(id, xport::chat_accept(l.listener, xport::ACCEPT_TIMEOUT, rx, secure, emitter(id)));
                    }
                    Err(e) => {
                        super::print(win, &format!("* Could not start the chat: {e}."));
                        fail_now(id, o.conn, &e);
                        return;
                    }
                }
            } else {
                super::print(win, &format!("* Connecting to {} at {ip}:{port}...", o.nick));
                spawn_for(id, xport::chat_connect(ip, port, info.server, rx, secure, emitter(id)));
            }
            open_window();
            ui::focus_input(hw(win)); // the prompt just closed; typing should go to the new chat
            ui::activate(hw(win));
        }
    }
}

/// Append one record to `transfers.log` in the log folder (kept regardless of the conversation
/// logging switch: it is a record of what files came and went, not of what people said).
fn log_transfer(conn: u32, event: &str, kind: XKind, peer: &str, name: &str, bytes: u64, size: u64, detail: &str) {
    let kind = match kind {
        XKind::Send => "Send",
        XKind::Receive => "Receive",
        XKind::Chat => "Chat",
    };
    let result = with(|a| {
        let host = a.conns.get(&conn).map(|c| c.host.clone()).unwrap_or_default();
        let path = logger::transfer_log_path(&log_base(&a.settings.log_dir));
        let text = logger::transfer_text(&logger::TransferEntry { event, kind, peer, host: &host, name, bytes, size, detail });
        a.logger.append(&path, &datetime_stamp(), &text)
    });
    if let Err(e) = result {
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !WARNED.swap(true, Ordering::Relaxed) {
            server_print(conn, &format!("* Could not write the transfer log ({e}); check the log folder in Preferences."));
        }
    }
}

/// Log an offer that never became a transfer (declined by the user, or refused by a safety rule).
fn log_offer(o: &Offer, event: &str, name: &str, size: u64, detail: &str) {
    let kind = if matches!(o.req, proto::Dcc::Chat { .. }) { XKind::Chat } else { XKind::Receive };
    log_transfer(o.conn, event, kind, &o.nick, name, 0, size, detail);
}

fn fail_now(id: u32, conn: u32, reason: &str) {
    let info = with(|a| {
        a.dcc.xfers.get_mut(&id).map(|x| {
            x.status = Status::Failed(reason.to_owned());
            (x.kind, x.peer.clone(), x.name.clone(), x.bytes, x.total)
        })
    });
    if let Some((kind, peer, name, bytes, total)) = info {
        log_transfer(conn, "Failed", kind, &peer, &name, bytes, total, reason);
    }
    release_mapping(id);
    server_print(conn, &format!("* DCC failed: {reason}."));
}

// ---------------------------------------------------------------- outgoing

/// Offer `path` to `nick`: listen, send the CTCP offer, and send the file to whoever connects.
pub fn send_file(h: HWND, conn: u32, nick: &str, path: PathBuf) {
    send_file_mode(h, conn, nick, path, false, false, false);
}

/// `force_passive` always makes a reverse offer; otherwise one is made automatically when we are
/// behind a router with no public address configured (the only kind of offer that could work).
pub fn send_file_mode(h: HWND, conn: u32, nick: &str, path: PathBuf, force_passive: bool, turbo: bool, secure: bool) {
    let Some(client) = registered_client(h, conn) else { return };
    if too_busy() {
        local(h, "* Too many DCC transfers are running.");
        return;
    }
    let size = match xport::file_size(&path) {
        Ok(s) => s,
        Err(e) => return local(h, &format!("* {e}")),
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name.is_empty() {
        return local(h, "* That path has no file name.");
    }
    let info = client.conn_info();
    let ip = advertise_ip(&client);
    let passive = force_passive || ip.as_ref().map_or(true, |ip| behind_nat(*ip, info.server));
    // Behind a router with UPnP switched on: try to make an ordinary offer reachable first.
    let mut mapped = None;
    if passive && !force_passive {
        if let Ok(ipv) = &ip {
            if upnp_wanted(*ipv, info.server) {
                match listen_upnp(*ipv) {
                    Ok(l) => mapped = Some(l),
                    Err(e) => local(h, &format!("* Port forwarding: {e}; offering passively instead.")),
                }
            }
        }
    }
    let passive = passive && mapped.is_none();
    if passive {
        let token = new_token();
        let offer = proto::passive_send_offer_kind(turbo, secure, &name, size, &token);
        send(conn, Message::new("PRIVMSG", &[nick, &offer.encode()]));
        let id = new_xfer(conn, XKind::Send, nick, &name, Some(path), size);
        with(|a| {
            if let Some(x) = a.dcc.xfers.get_mut(&id) {
                x.passive_token = Some(token);
                x.turbo = turbo;
                x.secure = secure;
                x.resume_pos = Some(Arc::new(AtomicU64::new(0)));
            }
        });
        watch_reply(id, false);
        let why = if force_passive { "" } else { " (your address is private, so you can't be reached directly)" };
        local(
            h,
            &format!(
                "* Offered {name} ({}) to {nick} as a passive (reverse) transfer{why}; waiting up to 2 minutes \
                 for them to accept and listen.",
                human_size(size)
            ),
        );
        open_window();
        return;
    }
    let l = match mapped {
        Some(l) => l,
        None => {
            let Ok(ip) = ip else { return };
            match listen_plain(ip) {
                Ok(l) => l,
                Err(e) => return local(h, &format!("* {e}.")),
            }
        }
    };
    let Listening { listener, port, ip: offer_ip, mapping } = l;
    let via = match &mapping {
        Some(m) => format!(" (forwarded through your router with {} as {offer_ip})", m.method.name()),
        None => String::new(),
    };
    let offer = proto::send_offer_kind(turbo, secure, &name, offer_ip, port, size, None);
    send(conn, Message::new("PRIVMSG", &[nick, &offer.encode()]));
    let id = new_xfer(conn, XKind::Send, nick, &name, Some(path.clone()), size);
    let resume = Arc::new(AtomicU64::new(0));
    with(|a| {
        if let Some(x) = a.dcc.xfers.get_mut(&id) {
            x.listen_port = port;
            x.resume_pos = Some(resume.clone());
            x.mapping = mapping;
        }
    });
    spawn_for(id, xport::send_file(path, listener, xport::ACCEPT_TIMEOUT, resume, turbo, secure, emitter(id)));
    local(h, &format!("* Offered {name} ({}) to {nick} on port {port}{via}; waiting up to 2 minutes for them to accept.", human_size(size)));
    open_window();
}

/// User-menu / `/dcc send nick` without a path: pick the file first.
pub fn send_file_dialog(h: HWND, conn: u32, nick: &str) {
    if let Some(path) = dialogs::pick_open_file(ui::frame(), &format!("Choose a file to send to {nick}")) {
        send_file(h, conn, nick, path);
    }
}

/// Offer a DCC chat to `nick`: listen, send the offer, open the chat window.
pub fn chat_start(h: HWND, conn: u32, nick: &str) {
    chat_start_mode(h, conn, nick, false, false);
}

/// Like [`send_file_mode`]: passive when forced, or automatically when we can't be reached.
pub fn chat_start_mode(h: HWND, conn: u32, nick: &str, force_passive: bool, secure: bool) {
    let Some(client) = registered_client(h, conn) else { return };
    if too_busy() {
        return local(h, "* Too many DCC transfers are running.");
    }
    if eq_ic(nick, &nick_of(conn)) {
        return local(h, "* You can't chat with yourself.");
    }
    let info = client.conn_info();
    let ip = advertise_ip(&client);
    let passive = force_passive || ip.as_ref().map_or(true, |ip| behind_nat(*ip, info.server));
    let mut mapped = None;
    if passive && !force_passive {
        if let Ok(ipv) = &ip {
            if upnp_wanted(*ipv, info.server) {
                match listen_upnp(*ipv) {
                    Ok(l) => mapped = Some(l),
                    Err(e) => local(h, &format!("* Port forwarding: {e}; offering a passive chat instead.")),
                }
            }
        }
    }
    if passive && mapped.is_none() {
        let token = new_token();
        let offer = proto::passive_chat_offer_kind(secure, &token);
        send(conn, Message::new("PRIVMSG", &[nick, &offer.encode()]));
        let id = new_xfer(conn, XKind::Chat, nick, &format!("chat with {nick}"), None, 0);
        let win = chat_window(conn, id, nick);
        super::print(win, &format!("* Waiting up to 2 minutes for {nick} to accept the passive chat and listen..."));
        let (handle, rx) = xport::chat_channel();
        with(|a| {
            if let Some(x) = a.dcc.xfers.get_mut(&id) {
                x.chat = Some(handle);
                x.chat_rx = Some(rx);
                x.passive_token = Some(token);
                x.secure = secure;
            }
        });
        watch_reply(id, true);
        open_window();
        ui::activate(hw(win));
        ui::focus_input(hw(win));
        return;
    }
    let l = match mapped {
        Some(l) => l,
        None => {
            let Ok(ip) = ip else { return };
            match listen_plain(ip) {
                Ok(l) => l,
                Err(e) => return local(h, &format!("* {e}.")),
            }
        }
    };
    let Listening { listener, port, ip: offer_ip, mapping } = l;
    let offer = proto::chat_offer_kind(secure, offer_ip, port);
    send(conn, Message::new("PRIVMSG", &[nick, &offer.encode()]));
    let id = new_xfer(conn, XKind::Chat, nick, &format!("chat with {nick}"), None, 0);
    with(|a| {
        if let Some(x) = a.dcc.xfers.get_mut(&id) {
            x.mapping = mapping;
        }
    });
    let win = chat_window(conn, id, nick);
    super::print(win, &format!("* Waiting up to 2 minutes for {nick} to accept the chat (port {port})..."));
    let (handle, rx) = xport::chat_channel();
    with(|a| {
        if let Some(x) = a.dcc.xfers.get_mut(&id) {
            x.chat = Some(handle);
        }
    });
    spawn_for(id, xport::chat_accept(listener, xport::ACCEPT_TIMEOUT, rx, secure, emitter(id)));
    open_window();
    ui::activate(hw(win));
    ui::focus_input(hw(win));
}

fn chat_window(conn: u32, id: u32, nick: &str) -> isize {
    let h = ui::create_child_ex(Kind::DccChat, conn, &format!("={nick}"), true);
    if let Some(c) = ui::child(h) {
        c.dcc = id;
    }
    let win = raw(h);
    with(|a| {
        if let Some(x) = a.dcc.xfers.get_mut(&id) {
            x.chat_win = win;
        }
    });
    win
}

/// The user typed a line in a DCC chat window.
pub fn chat_say(h: HWND, text: &str) {
    let Some((conn, id)) = ui::child(h).map(|c| (c.conn, c.dcc)) else { return };
    let ok = with(|a| a.dcc.xfers.get(&id).and_then(|x| x.chat.as_ref()).is_some_and(|c| c.send(text)));
    if ok {
        super::print_k(raw(h), LineKind::Normal, &format!("<{}> {text}", nick_of(conn)));
    } else {
        local(h, "* The chat is not connected.");
    }
}

/// `/me` in a DCC chat window.
pub fn chat_action(h: HWND, text: &str) {
    let Some((conn, id)) = ui::child(h).map(|c| (c.conn, c.dcc)) else { return };
    let ok = with(|a| {
        a.dcc.xfers.get(&id).and_then(|x| x.chat.as_ref()).is_some_and(|c| c.send(&format!("\x01ACTION {text}\x01")))
    });
    if ok {
        super::print_k(raw(h), LineKind::Action, &format!("* {} {text}", nick_of(conn)));
    } else {
        local(h, "* The chat is not connected.");
    }
}

/// A DCC chat window is closing: end the chat.
pub fn chat_closed(h: HWND) {
    let Some(id) = ui::child(h).map(|c| c.dcc) else { return };
    let ended = with(|a| {
        a.dcc.xfers.get_mut(&id).and_then(|x| {
            if let Some(ab) = x.abort.take() {
                ab.abort();
            }
            x.chat = None; // dropping the handle ends the chat on our side
            x.chat_win = 0;
            let was_running = x.status.running();
            if was_running {
                x.status = Status::Done;
            }
            was_running.then(|| (x.conn, x.peer.clone(), x.name.clone()))
        })
    });
    if let Some((conn, peer, name)) = ended {
        log_transfer(conn, "Done", XKind::Chat, &peer, &name, 0, 0, "window closed by you");
    }
    release_mapping(id);
    super::dccwin_refresh();
}

// ---------------------------------------------------------------- events from the tasks

/// Handle everything the transfer tasks have reported (called from the UI thread).
pub(super) fn drain_events() {
    loop {
        let next = DCC_QUEUE.lock().unwrap().pop_front();
        let Some((id, ev)) = next else { break };
        on_event(id, ev);
    }
}

fn on_event(id: u32, ev: DccEvent) {
    let Some((conn, kind, peer, name, win)) =
        with(|a| a.dcc.xfers.get(&id).map(|x| (x.conn, x.kind, x.peer.clone(), x.name.clone(), x.chat_win)))
    else {
        return;
    };
    match ev {
        DccEvent::Progress { bytes, total } => with(|a| {
            if let Some(x) = a.dcc.xfers.get_mut(&id) {
                if x.status.running() {
                    x.bytes = bytes;
                    x.total = total;
                    x.status = Status::Active;
                }
            }
        }),
        DccEvent::Done { bytes } => {
            let total = with(|a| {
                a.dcc.xfers.get_mut(&id).map(|x| {
                    x.bytes = bytes;
                    x.status = Status::Done;
                    x.abort = None;
                    x.total
                })
            });
            log_transfer(conn, "Done", kind, &peer, &name, bytes, total.unwrap_or(bytes), "");
            let verb = if kind == XKind::Send { "Sent" } else { "Received" };
            server_print(conn, &format!("* DCC: {verb} {name} ({}) {} {peer}.", human_size(bytes), if kind == XKind::Send { "to" } else { "from" }));
        }
        DccEvent::Failed { reason } => {
            // A task that was cancelled has already been logged as Cancelled.
            let was_running = with(|a| {
                a.dcc.xfers.get_mut(&id).map(|x| {
                    let running = x.status.running();
                    if running {
                        x.status = Status::Failed(reason.clone());
                    }
                    x.abort = None;
                    (running, x.bytes, x.total)
                })
            });
            if let Some((true, bytes, total)) = was_running {
                log_transfer(conn, "Failed", kind, &peer, &name, bytes, total, &reason);
            }
            server_print(conn, &format!("* DCC with {peer} failed ({name}): {reason}."));
        }
        DccEvent::Encrypted { detail } => {
            with(|a| {
                if let Some(x) = a.dcc.xfers.get_mut(&id) {
                    x.encrypted = true;
                }
            });
            let line = format!("* DCC with {peer} is encrypted ({detail}); the other side is not verified.");
            if win != 0 {
                super::print(win, &line);
            } else {
                server_print(conn, &line);
            }
        }
        DccEvent::ChatConnected => {
            with(|a| {
                if let Some(x) = a.dcc.xfers.get_mut(&id) {
                    x.status = Status::Active;
                }
            });
            if win != 0 {
                super::print(win, &format!("* Connected to {peer}. Everything here goes directly to them."));
            }
        }
        DccEvent::ChatLine(text) => {
            if win != 0 {
                match text.strip_prefix("\x01ACTION ") {
                    Some(rest) => super::print_k(win, LineKind::Action, &format!("* {peer} {}", rest.trim_end_matches('\x01'))),
                    None => super::print_k(win, LineKind::Normal, &format!("<{peer}> {text}")),
                }
            }
        }
        DccEvent::ChatClosed { reason } => {
            let outcome = with(|a| {
                a.dcc.xfers.get_mut(&id).and_then(|x| {
                    let was_running = x.status.running();
                    if was_running {
                        x.status = if x.bytes == 0 && matches!(x.status, Status::Waiting) {
                            Status::Failed(reason.clone())
                        } else {
                            Status::Done
                        };
                    }
                    x.chat = None;
                    x.abort = None;
                    was_running.then(|| matches!(x.status, Status::Done))
                })
            });
            if let Some(connected) = outcome {
                log_transfer(conn, if connected { "Done" } else { "Failed" }, kind, &peer, &name, 0, 0, &reason);
            }
            if win != 0 {
                super::print(win, &format!("* Chat ended: {reason}."));
            }
        }
    }
    release_mapping(id);
    super::dccwin_refresh();
}

// ---------------------------------------------------------------- the Transfers window

/// One row of the Transfers window.
pub struct Row {
    pub id: u32,
    pub direction: &'static str,
    pub peer: String,
    pub name: String,
    pub size: String,
    pub progress: String,
    pub speed: String,
    pub status: String,
}

pub fn rows() -> Vec<Row> {
    with(|a| {
        a.dcc
            .xfers
            .values()
            .map(|x| {
                let secs = x.started.elapsed().as_secs_f64().max(0.001);
                let active = x.status == Status::Active && x.kind != XKind::Chat;
                Row {
                    id: x.id,
                    direction: match x.kind {
                        XKind::Send => "Send",
                        XKind::Receive => "Receive",
                        XKind::Chat => "Chat",
                    },
                    peer: x.peer.clone(),
                    name: x.name.clone(),
                    size: if x.kind == XKind::Chat { String::new() } else { human_size(x.total) },
                    progress: if x.kind == XKind::Chat || x.total == 0 && x.bytes == 0 {
                        String::new()
                    } else if x.total == 0 {
                        "100%".to_owned()
                    } else {
                        format!("{}%", (x.bytes as f64 / x.total as f64 * 100.0).min(100.0) as u32)
                    },
                    speed: if active {
                        format!("{}/s", human_size((x.bytes.saturating_sub(x.base) as f64 / secs) as u64))
                    } else {
                        String::new()
                    },
                    status: match &x.status {
                        Status::Waiting if x.resume_wait.is_some() => "Waiting for resume reply".to_owned(),
                        Status::Waiting => "Waiting".to_owned(),
                        Status::Active => {
                            let base = if x.kind == XKind::Chat { "Connected" } else { "Transferring" };
                            if x.encrypted { format!("{base} (encrypted)") } else { base.to_owned() }
                        }
                        Status::Done => "Done".to_owned(),
                        Status::Failed(r) => format!("Failed: {r}"),
                        Status::Cancelled => "Cancelled".to_owned(),
                    },
                }
            })
            .collect()
    })
}

/// Cancel a running transfer or chat (a half-written download is deleted by the task's cleanup).
pub fn cancel(id: u32) {
    let win = with(|a| {
        a.dcc.xfers.get_mut(&id).and_then(|x| {
            if !x.status.running() {
                return None;
            }
            if let Some(ab) = x.abort.take() {
                ab.abort();
            }
            x.chat = None;
            x.resume_wait = None;
            x.status = Status::Cancelled;
            let part = (x.kind == XKind::Receive).then(|| x.path.clone()).flatten();
            Some((x.chat_win, x.peer.clone(), x.conn, x.name.clone(), part, x.kind, x.bytes, x.total))
        })
    });
    release_mapping(id);
    if let Some((win, peer, conn, name, part, kind, bytes, total)) = win {
        log_transfer(conn, "Cancelled", kind, &peer, &name, bytes, total, "cancelled by you");
        if win != 0 {
            super::print(win, &format!("* You ended the chat with {peer}."));
        }
        if let Some(p) = part {
            let part = xport::part_path(&p);
            if std::fs::metadata(&part).map(|m| m.len() > 0).unwrap_or(false) {
                server_print(conn, &format!("* Cancelled {name}; the partial download is kept as {} so you can resume it.", part.display()));
            }
        }
    }
    super::dccwin_refresh();
}

pub fn clear_finished() {
    with(|a| a.dcc.xfers.retain(|_, x| x.status.running()));
    super::dccwin_refresh();
}

/// What may happen when the user asks to open a transfer's file.
#[derive(Debug, PartialEq, Eq)]
enum OpenCheck {
    Open,
    /// Program or script types: only after an explicit confirmation (default No).
    AskFirst,
    Refuse(&'static str),
}

fn open_check(kind: XKind, status: &Status, exists: bool, risky: bool) -> OpenCheck {
    if kind != XKind::Receive || *status != Status::Done {
        OpenCheck::Refuse("Only a download that has finished can be opened.")
    } else if !exists {
        OpenCheck::Refuse("That file is no longer where it was saved.")
    } else if risky {
        OpenCheck::AskFirst
    } else {
        OpenCheck::Open
    }
}

/// Open a finished download with the program Windows associates with it. Programs and scripts
/// are opened only after a confirmation that defaults to No.
pub fn open_file(id: u32) {
    let Some((kind, status, path, conn, peer, name, bytes)) =
        with(|a| a.dcc.xfers.get(&id).map(|x| (x.kind, x.status.clone(), x.path.clone(), x.conn, x.peer.clone(), x.name.clone(), x.bytes)))
    else {
        return;
    };
    let Some(path) = path else { return };
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let owner = ui::frame();
    match open_check(kind, &status, path.is_file(), proto::is_risky_extension(&file)) {
        OpenCheck::Refuse(why) => dialogs::info(owner, "Open file", why),
        OpenCheck::AskFirst => {
            let text = format!(
                "{file} is a program or script type, and it came from {peer} over DCC. Opening it can harm your computer.\n\n\
                 Open it anyway?"
            );
            if dialogs::confirm(owner, "Open a program from DCC", &text) {
                log_transfer(conn, "Opened", kind, &peer, &name, bytes, bytes, "confirmed a program or script type");
                shell_open(&path);
            }
        }
        OpenCheck::Open => {
            log_transfer(conn, "Opened", kind, &peer, &name, bytes, bytes, "");
            shell_open(&path);
        }
    }
}

/// Open the transfer log in the default text viewer.
pub fn open_log() {
    let path = logger::transfer_log_path(&log_base(&settings().log_dir));
    if path.exists() {
        shell_open(&path);
    } else {
        dialogs::info(ui::frame(), "Transfer log", "Nothing has been logged yet. Transfers, declined and refused offers are recorded in transfers.log in the log folder.");
    }
}

/// Open Explorer on a received/sent file.
pub fn show_in_folder(id: u32) {
    let Some(path) = with(|a| a.dcc.xfers.get(&id).and_then(|x| x.path.clone())) else { return };
    let params = wide(&format!("/select,\"{}\"", path.display()));
    unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            windows::core::w!("open"),
            windows::core::w!("explorer.exe"),
            windows::core::PCWSTR(params.as_ptr()),
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// `/dcc portmap` (or `/dcc upnp`): find the router, open a test port, read the external address
/// and close the port again, saying which method worked. Changes nothing permanently.
fn upnp_test(h: HWND) {
    local(h, "* Port forwarding: looking for a router that supports PCP, NAT-PMP or UPnP (this can take a few seconds)...");
    let (listener, port) = match listen_port(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)) {
        Ok(v) => v,
        Err(e) => return local(h, &format!("* Port forwarding: {e}.")),
    };
    let options = portmap_options();
    match with(|a| a.rt.block_on(portmap::open(&options, port, 120))) {
        Ok(m) => {
            let (ext, method) = (m.external_ip, m.method.name());
            let closed = with(|a| a.rt.block_on(m.close()));
            drop(listener);
            local(
                h,
                &format!(
                    "* Port forwarding works through {method}: your router reports the external address {ext}. A test port ({port}) was opened and {}.",
                    if closed.is_ok() { "removed again".to_owned() } else { format!("could not be removed ({}); it expires by itself", closed.unwrap_err()) }
                ),
            );
        }
        Err(e) => local(h, &format!("* Port forwarding: {e}.")),
    }
}

/// `/dcc send <nick> [file]`, `/dcc chat <nick>`, `/dcc list`.
pub fn command(h: HWND, conn: u32, rest: &str) {
    let mut parts = rest.trim().splitn(3, ' ');
    let sub = parts.next().unwrap_or("").to_ascii_lowercase();
    let nick = parts.next().unwrap_or("").trim();
    let tail = parts.next().unwrap_or("").trim().trim_matches('"');
    match sub.as_str() {
        "" | "list" => open_window(),
        "send" if !nick.is_empty() => {
            if tail.is_empty() {
                send_file_dialog(h, conn, nick);
            } else {
                send_file(h, conn, nick, PathBuf::from(tail));
            }
        }
        "chat" if !nick.is_empty() => chat_start(h, conn, nick),
        // pchat: passive; schat: secure (TLS); pschat: both
        "pchat" | "schat" | "pschat" if !nick.is_empty() => chat_start_mode(h, conn, nick, sub != "schat", sub != "pchat"),
        // psend: passive; tsend: turbo (no acknowledgements); ptsend: both; ssend: secure (TLS); pssend: both
        "psend" | "tsend" | "ptsend" | "ssend" | "pssend" if !nick.is_empty() => {
            let path = if tail.is_empty() {
                dialogs::pick_open_file(ui::frame(), &format!("Choose a file to send to {nick}"))
            } else {
                Some(PathBuf::from(tail))
            };
            if let Some(p) = path {
                let (passive, turbo, secure) = match sub.as_str() {
                    "psend" => (true, false, false),
                    "tsend" => (false, true, false),
                    "ptsend" => (true, true, false),
                    "ssend" => (false, false, true),
                    _ => (true, false, true),
                };
                send_file_mode(h, conn, nick, p, passive, turbo, secure);
            }
        }
        "upnp" | "portmap" => upnp_test(h),
        "allow" if !nick.is_empty() => {
            let was = with(|a| a.dcc.declines.allow(nick));
            local(h, &if was { format!("* DCC offers from {nick} are allowed again.") } else { format!("* {nick} wasn't being ignored for DCC.") });
        }
        _ => local(h, "* Usage: /dcc send <nick> [file]   /dcc chat <nick>   /dcc psend|pchat <nick> (passive, for when you are behind a router)   /dcc tsend|ptsend <nick> [file] (turbo)   /dcc ssend|pssend <nick> [file], /dcc schat|pschat <nick> (encrypted)   /dcc allow <nick>   /dcc portmap (test your router)   /dcc list"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upnp_is_used_only_when_enabled_ipv4_and_really_behind_a_router() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let public_server = Some(ip("93.184.216.34"));
        let lan_server = Some(ip("192.168.1.5"));
        // the normal case: switched on, private address, server on the internet
        assert!(upnp_rule(true, true, ip("192.168.1.20"), public_server));
        // switched off
        assert!(!upnp_rule(false, true, ip("192.168.1.20"), public_server));
        // the user has set a public address: they handle forwarding themselves
        assert!(!upnp_rule(true, false, ip("192.168.1.20"), public_server));
        // already reachable
        assert!(!upnp_rule(true, true, ip("93.184.216.99"), public_server));
        // a server on the LAN can reach us directly
        assert!(!upnp_rule(true, true, ip("192.168.1.20"), lan_server));
        // UPnP IGD is an IPv4 mechanism
        assert!(!upnp_rule(true, true, ip("fd00::5"), public_server));
        // unknown server address is treated as public (the safe default)
        assert!(upnp_rule(true, true, ip("10.0.0.2"), None));
    }

    #[test]
    fn only_finished_downloads_open_and_programs_need_confirmation() {
        let done = Status::Done;
        assert_eq!(open_check(XKind::Receive, &done, true, false), OpenCheck::Open);
        assert_eq!(open_check(XKind::Receive, &done, true, true), OpenCheck::AskFirst);
        for status in [Status::Waiting, Status::Active, Status::Cancelled, Status::Failed("x".into())] {
            assert!(matches!(open_check(XKind::Receive, &status, true, false), OpenCheck::Refuse(_)), "{status:?}");
        }
        // our own uploads and chats are not "received files"
        assert!(matches!(open_check(XKind::Send, &done, true, false), OpenCheck::Refuse(_)));
        assert!(matches!(open_check(XKind::Chat, &done, true, false), OpenCheck::Refuse(_)));
        // a file that was moved or deleted
        assert!(matches!(open_check(XKind::Receive, &done, false, false), OpenCheck::Refuse(_)));
        assert!(matches!(open_check(XKind::Receive, &done, false, true), OpenCheck::Refuse(_)));
    }

    #[test]
    fn repeated_declines_block_a_nick_until_allowed() {
        let mut d = Declines::default();
        assert!(!d.blocked("Spam"));
        assert!(!d.record("Spam") && !d.record("spam"), "no notice before the limit");
        assert!(!d.blocked("SPAM"));
        assert!(d.record("sPam"), "the limit is reached on the third decline (notice once)");
        assert!(d.blocked("spam") && d.blocked("SPAM"), "case-insensitive");
        assert!(!d.record("spam"), "past the limit there is no second notice");
        assert!(d.blocked("spam"));
        assert!(!d.blocked("someone-else"));
        assert!(d.allow("Spam"));
        assert!(!d.blocked("spam"));
        assert!(!d.allow("spam"), "nothing left to allow");
    }
}
