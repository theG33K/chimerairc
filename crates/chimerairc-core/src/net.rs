//! Async transport: TCP (+ TLS via rustls) driving a [`Session`].
//!
//! Call [`spawn`] from inside a tokio runtime. It returns a [`Client`] handle for
//! sending and a receiver of [`Event`]s; the receiver yields `Event::Disconnected`
//! as its last item.

use crate::{Config, Event, IgnoreRule, Session};
use std::io;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{interval, timeout, Instant};
use chimerairc_proto::{Message, MAX_LINE_LEN, MAX_TAGS_LEN};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const PING_AFTER: Duration = Duration::from_secs(120);
const DEAD_AFTER: Duration = Duration::from_secs(240);
const QUIT_GRACE: Duration = Duration::from_secs(5);
/// Longest line we will buffer: tags + body.
const MAX_INCOMING: usize = MAX_TAGS_LEN + MAX_LINE_LEN;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

enum Cmd {
    SetIgnores(Vec<IgnoreRule>),
    Watch(Vec<String>),
    Send(Message),
    Typing(String, crate::event::TypingState),
    Quit(Option<String>),
}

/// Cheap, cloneable handle for sending to the server. Lines are validated
/// (no CR/LF/NUL, length-capped) when written; invalid ones surface as `Event::Error`.
#[derive(Clone)]
pub struct Client {
    tx: mpsc::UnboundedSender<Cmd>,
    info: Arc<Mutex<ConnInfo>>,
}

/// The two ends of the IRC connection. DCC needs both: our address to put in offers, and the
/// server's to decide whether a local/private DCC peer is plausible (see `target_allowed`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnInfo {
    /// Our address as seen on the connection to the server.
    pub local: Option<IpAddr>,
    /// The IRC server's address.
    pub server: Option<IpAddr>,
}

impl Client {
    /// Addresses of the live connection (empty until connected).
    pub fn conn_info(&self) -> ConnInfo {
        *self.info.lock().unwrap()
    }

    /// Queue a message. Returns false if the connection task has ended.
    pub fn send(&self, m: Message) -> bool {
        self.tx.send(Cmd::Send(m)).is_ok()
    }
    pub fn privmsg(&self, target: &str, text: &str) -> bool {
        self.send(Message::new("PRIVMSG", &[target, text]))
    }
    pub fn join(&self, channel: &str) -> bool {
        self.send(Message::new("JOIN", &[channel]))
    }
    pub fn part(&self, channel: &str) -> bool {
        self.send(Message::new("PART", &[channel]))
    }
    /// Replace the ignore rules of a live connection.
    pub fn set_ignores(&self, rules: Vec<IgnoreRule>) -> bool {
        self.tx.send(Cmd::SetIgnores(rules)).is_ok()
    }

    /// Replace the list of nicknames whose online state is reported as `Event::Presence`.
    pub fn watch(&self, nicks: Vec<String>) -> bool {
        self.tx.send(Cmd::Watch(nicks)).is_ok()
    }

    /// Tell a channel or nick whether we are typing (the +typing tag; nothing is sent unless the server allows it).
    pub fn typing(&self, target: &str, state: crate::event::TypingState) -> bool {
        self.tx.send(Cmd::Typing(target.to_owned(), state)).is_ok()
    }

    /// Send QUIT and close after the server hangs up (or a short grace period).
    pub fn quit(&self, reason: Option<&str>) -> bool {
        self.tx.send(Cmd::Quit(reason.map(str::to_owned))).is_ok()
    }
}

pub fn spawn(cfg: Config) -> (Client, mpsc::UnboundedReceiver<Event>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let info = Arc::new(Mutex::new(ConnInfo::default()));
    let task_info = info.clone();
    tokio::spawn(async move {
        let reason = match run(cfg, cmd_rx, &ev_tx, task_info).await {
            Ok(r) => r,
            Err(e) => e.to_string(),
        };
        let _ = ev_tx.send(Event::Disconnected { reason });
    });
    (Client { tx: cmd_tx, info }, ev_rx)
}

/// The TLS client settings for a connection: the public roots plus any extra ones from the
/// configuration, and the client certificate when there is one.
pub(crate) fn tls_config(cfg: &Config) -> io::Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for extra in &cfg.extra_roots {
        roots.add(extra.clone()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("bad extra root certificate: {e}")))?;
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_root_certificates(roots);
    match &cfg.client_cert {
        Some(c) => {
            let (chain, key) = c.parts();
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("the client certificate cannot be used: {e}")))
        }
        None => Ok(builder.with_no_client_auth()),
    }
}

async fn connect(cfg: &Config, info: &Mutex<ConnInfo>) -> io::Result<Box<dyn Io>> {
    let tcp = TcpStream::connect((cfg.host.as_str(), cfg.port)).await?;
    tcp.set_nodelay(true)?;
    *info.lock().unwrap() = ConnInfo {
        local: tcp.local_addr().ok().map(|a| a.ip()),
        server: tcp.peer_addr().ok().map(|a| a.ip()),
    };
    if !cfg.tls {
        return Ok(Box::new(tcp));
    }
    let conf = tls_config(cfg)?;
    let name = rustls::pki_types::ServerName::try_from(cfg.host.clone())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let tls = tokio_rustls::TlsConnector::from(Arc::new(conf)).connect(name, tcp).await?;
    Ok(Box::new(tls))
}

/// Reads CRLF/LF-terminated lines with a hard size cap; overlong lines are dropped.
async fn read_lines<R: AsyncRead + Unpin>(rd: R, tx: mpsc::Sender<Result<String, String>>) {
    let mut rd = BufReader::new(rd);
    let mut buf = Vec::new();
    let closed = || Err("connection closed by server".to_owned());
    loop {
        buf.clear();
        let n = match (&mut rd).take(MAX_INCOMING as u64).read_until(b'\n', &mut buf).await {
            Ok(0) => return drop(tx.send(closed()).await),
            Ok(n) => n,
            Err(e) => return drop(tx.send(Err(e.to_string())).await),
        };
        if buf.last() != Some(&b'\n') {
            if n < MAX_INCOMING {
                return drop(tx.send(closed()).await); // EOF mid-line
            }
            // Overlong: discard through the next newline.
            loop {
                buf.clear();
                match (&mut rd).take(MAX_INCOMING as u64).read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => return drop(tx.send(closed()).await),
                    Ok(_) if buf.last() == Some(&b'\n') => break,
                    Ok(_) => {}
                }
            }
            continue;
        }
        let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_owned();
        if tx.send(Ok(line)).await.is_err() {
            return;
        }
    }
}

async fn write_msg<W: AsyncWrite + Unpin>(
    wr: &mut W,
    m: &Message,
    ev_tx: &mpsc::UnboundedSender<Event>,
) -> io::Result<()> {
    match m.encode_wire() {
        Ok(mut line) => {
            line.push_str("\r\n");
            wr.write_all(line.as_bytes()).await?;
            wr.flush().await
        }
        Err(e) => {
            let _ = ev_tx.send(Event::Error { text: format!("not sent ({e}): {}", m.command) });
            Ok(())
        }
    }
}

async fn pump<W: AsyncWrite + Unpin>(
    s: &mut Session,
    wr: &mut W,
    ev_tx: &mpsc::UnboundedSender<Event>,
) -> io::Result<()> {
    for m in s.take_outbox() {
        write_msg(wr, &m, ev_tx).await?;
    }
    for e in s.take_events() {
        let _ = ev_tx.send(e);
    }
    Ok(())
}

async fn run(
    cfg: Config,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    ev_tx: &mpsc::UnboundedSender<Event>,
    info: Arc<Mutex<ConnInfo>>,
) -> io::Result<String> {
    let stream = timeout(CONNECT_TIMEOUT, connect(&cfg, &info))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connection timed out"))??;
    let _ = ev_tx.send(Event::Connected);
    let (rd, mut wr) = tokio::io::split(stream);
    let (line_tx, mut line_rx) = mpsc::channel(256);
    tokio::spawn(read_lines(rd, line_tx));

    let mut session = Session::new(cfg);
    session.start();
    pump(&mut session, &mut wr, ev_tx).await?;

    let mut last_rx = Instant::now();
    let mut pinged = false;
    let mut quit_deadline: Option<Instant> = None;
    let mut tick = interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            line = line_rx.recv() => match line {
                Some(Ok(l)) => {
                    last_rx = Instant::now();
                    pinged = false;
                    if let Ok(m) = Message::parse(&l) {
                        session.handle(&m);
                    }
                    // Drain whatever else is already queued so state snapshots coalesce.
                    let mut closing = None;
                    for _ in 0..512 {
                        match line_rx.try_recv() {
                            Ok(Ok(l)) => {
                                if let Ok(m) = Message::parse(&l) {
                                    session.handle(&m);
                                }
                            }
                            Ok(Err(reason)) => {
                                closing = Some(reason);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    pump(&mut session, &mut wr, ev_tx).await?;
                    // The server wants TLS (STS): end this plaintext connection now; the caller
                    // reconnects securely from the StsUpgrade event.
                    if session.is_upgrading() {
                        return Ok("reconnecting with TLS (the server requires it)".into());
                    }
                    if let Some(reason) = closing {
                        return Ok(reason);
                    }
                }
                Some(Err(reason)) => return Ok(reason),
                None => return Ok("connection closed".into()),
            },
            cmd = cmd_rx.recv(), if quit_deadline.is_none() => {
                match cmd {
                    Some(Cmd::SetIgnores(rules)) => session.set_ignores(rules),
                    Some(Cmd::Watch(nicks)) => {
                        session.set_watch(&nicks);
                        pump(&mut session, &mut wr, ev_tx).await?;
                    }
                    Some(Cmd::Send(m)) => write_msg(&mut wr, &m, ev_tx).await?,
                    Some(Cmd::Typing(target, state)) => {
                        session.typing(&target, state);
                        pump(&mut session, &mut wr, ev_tx).await?;
                    }
                    Some(Cmd::Quit(r)) => {
                        let m = match r {
                            Some(r) => Message::new("QUIT", &[&r]),
                            None => Message::new("QUIT", &[]),
                        };
                        write_msg(&mut wr, &m, ev_tx).await?;
                        quit_deadline = Some(Instant::now() + QUIT_GRACE);
                    }
                    // Every handle dropped: leave politely.
                    None => {
                        write_msg(&mut wr, &Message::new("QUIT", &[]), ev_tx).await?;
                        quit_deadline = Some(Instant::now() + QUIT_GRACE);
                    }
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                if quit_deadline.is_some_and(|d| now >= d) {
                    return Ok("quit".into());
                }
                session.poll();
                pump(&mut session, &mut wr, ev_tx).await?;
                let idle = now.duration_since(last_rx);
                if idle >= DEAD_AFTER {
                    return Ok("ping timeout".into());
                }
                if idle >= PING_AFTER && !pinged {
                    pinged = true;
                    write_msg(&mut wr, &Message::new("PING", &["chimerairc"]), ev_tx).await?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    fn cert() -> crate::clientcert::ClientCert {
        let ck = rcgen::generate_simple_self_signed(vec!["me.example".to_owned()]).unwrap();
        let key = PrivateKeyDer::try_from(ck.signing_key.serialize_der()).unwrap();
        crate::clientcert::ClientCert::from_der(vec![ck.cert.der().clone()], key).unwrap()
    }

    #[test]
    fn the_client_certificate_is_offered_to_the_server_only_when_configured() {
        let plain = tls_config(&Config::new("h", "me")).unwrap();
        assert!(!plain.client_auth_cert_resolver.has_certs());
        let mut cfg = Config::new("h", "me");
        cfg.client_cert = Some(cert());
        let with = tls_config(&cfg).unwrap();
        assert!(with.client_auth_cert_resolver.has_certs());
    }

    #[test]
    fn extra_roots_are_accepted_and_bad_ones_refused() {
        let ck = rcgen::generate_simple_self_signed(vec!["srv.example".to_owned()]).unwrap();
        let mut cfg = Config::new("h", "me");
        cfg.extra_roots = vec![ck.cert.der().clone()];
        assert!(tls_config(&cfg).is_ok());
        cfg.extra_roots = vec![CertificateDer::from(vec![1, 2, 3, 4])];
        let e = tls_config(&cfg).err().expect("garbage is not a certificate");
        assert!(e.to_string().contains("root certificate"), "{e}");
    }
}
