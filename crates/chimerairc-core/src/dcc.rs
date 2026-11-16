//! DCC transports: send a file, receive a file, and a line-based chat, over plain TCP.
//!
//! Everything reports through an `emit` callback so the caller decides how to deliver events
//! (the GUI posts them to its message loop; tests collect them in a channel).
//!
//! Safety properties (all covered by tests):
//! * we only connect where [`chimerairc_proto::dcc::target_allowed`] says (no bounce attacks);
//! * a receiver never reads more than the size the offer declared;
//! * a failed, truncated or cancelled download leaves no partial file behind;
//! * acknowledgements are written without ever blocking, so a sender that ignores them cannot
//!   stall the download;
//! * a listener accepts exactly one connection and gives up after a timeout.

use std::io::{self, SeekFrom};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use chimerairc_proto::dcc;

use crate::dcctls::{self, BoxConn};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
pub const ACCEPT_TIMEOUT: Duration = Duration::from_secs(120);
/// Give up on a transfer that delivers nothing for this long.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// After the last byte, wait this long for the receiver's final acknowledgement.
const FINAL_ACK_WAIT: Duration = Duration::from_secs(5);
const CHUNK: usize = 64 * 1024;
/// Longest chat line we keep (longer ones are cut), and the longest we send.
pub const MAX_CHAT_LINE: usize = 2000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DccEvent {
    /// Bytes moved so far (received, or sent) out of `total`.
    Progress { bytes: u64, total: u64 },
    Done { bytes: u64 },
    Failed { reason: String },
    /// The chat peer connected.
    /// The connection is encrypted (secure DCC): `TLS 1.3 (...)`. The other side is *not* authenticated.
    Encrypted { detail: String },
    ChatConnected,
    ChatLine(String),
    ChatClosed { reason: String },
}

// ---------------------------------------------------------------- speed limit

/// The most bytes per second *each* file transfer may move; 0 = unlimited. It is read on every
/// chunk, so changing it affects transfers that are already running.
static RATE_LIMIT: AtomicU64 = AtomicU64::new(0);

pub fn set_rate_limit(bytes_per_sec: u64) {
    RATE_LIMIT.store(bytes_per_sec, Ordering::Relaxed);
}

/// How long to wait after having moved `bytes` in `elapsed` so the average stays at `limit`.
fn pace_delay(limit: u64, bytes: u64, elapsed: Duration) -> Duration {
    if limit == 0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(bytes as f64 / limit as f64).saturating_sub(elapsed)
}

/// Chunk size to use at `limit`: small enough that pauses stay short (and progress and cancelling
/// stay responsive) at low limits.
fn chunk_for(limit: u64) -> usize {
    if limit == 0 {
        CHUNK
    } else {
        (limit / 10).clamp(1024, CHUNK as u64) as usize
    }
}

/// Keeps one transfer at or under the current limit.
struct Throttle {
    start: Instant,
    bytes: u64,
    limit: u64,
}

impl Throttle {
    fn new() -> Throttle {
        Throttle { start: Instant::now(), bytes: 0, limit: RATE_LIMIT.load(Ordering::Relaxed) }
    }

    /// Chunk size for the next read at the current limit.
    fn chunk(&self) -> usize {
        chunk_for(RATE_LIMIT.load(Ordering::Relaxed))
    }

    /// Account for `n` more bytes and sleep if we are ahead of the allowed rate.
    async fn pace(&mut self, n: u64) {
        let limit = RATE_LIMIT.load(Ordering::Relaxed);
        if limit != self.limit {
            *self = Throttle { start: Instant::now(), bytes: 0, limit }; // a new limit starts a new window
        }
        if limit == 0 {
            return;
        }
        self.bytes += n;
        let elapsed = self.start.elapsed();
        let wait = pace_delay(limit, self.bytes, elapsed);
        if wait.is_zero() {
            // Behind schedule (a stall, or a slow disk): do not bank the lost time as a burst.
            let due = Duration::from_secs_f64(self.bytes as f64 / limit as f64);
            if elapsed > due + Duration::from_secs(1) {
                *self = Throttle { start: Instant::now(), bytes: 0, limit };
            }
        } else {
            tokio::time::sleep(wait).await;
        }
    }
}

// ---------------------------------------------------------------- listening

/// Bind a listener on all IPv4 interfaces, in `ports` (inclusive) if given, else on any free port.
pub async fn listen(ports: Option<(u16, u16)>) -> io::Result<(TcpListener, u16)> {
    listen_for(IpAddr::V4(Ipv4Addr::UNSPECIFIED), ports).await
}

/// Like [`listen`], but on all interfaces of the same family as `advertised` (the address that
/// will go into the offer): an IPv6 offer needs an IPv6 listener.
pub async fn listen_for(advertised: IpAddr, ports: Option<(u16, u16)>) -> io::Result<(TcpListener, u16)> {
    let any = match advertised {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(_) => IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
    };
    let bind = |p: u16| TcpListener::bind(SocketAddr::new(any, p));
    let Some((lo, hi)) = ports.filter(|(lo, hi)| lo <= hi && *lo >= 1024) else {
        let l = bind(0).await?;
        let p = l.local_addr()?.port();
        return Ok((l, p));
    };
    // Start somewhere in the range so concurrent offers do not all fight over the first port.
    let span = (hi - lo) as u32 + 1;
    let start = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0)) % span;
    let mut last = io::Error::new(io::ErrorKind::AddrInUse, "no free port in the DCC range");
    for i in 0..span {
        let p = lo + ((start + i) % span) as u16;
        match bind(p).await {
            Ok(l) => return Ok((l, p)),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Accept exactly one connection, then stop listening.
pub async fn accept_one(listener: TcpListener, wait: Duration) -> Result<TcpStream, String> {
    match timeout(wait, listener.accept()).await {
        Err(_) => Err("nobody connected before the offer timed out".into()),
        Ok(Err(e)) => Err(format!("accept failed: {e}")),
        Ok(Ok((stream, _peer))) => Ok(stream), // the listener is dropped here: one connection only
    }
}

async fn connect(ip: IpAddr, port: u16, irc_server: Option<IpAddr>) -> Result<TcpStream, String> {
    dcc::target_allowed(ip, port, irc_server).map_err(|why| format!("refused: {why}"))?;
    match timeout(CONNECT_TIMEOUT, TcpStream::connect(SocketAddr::new(ip, port))).await {
        Err(_) => Err("connection timed out".into()),
        Ok(Err(e)) => Err(format!("could not connect: {e}")),
        Ok(Ok(s)) => {
            let _ = s.set_nodelay(true);
            Ok(s)
        }
    }
}

/// Turn a fresh TCP connection into the stream a transfer or chat runs over: as it is, or wrapped in
/// TLS when the offer was secure. The peer that listened is the TLS server (`we_listened`), the one
/// that connected is the client. Reports the encryption through `emit` once it is in place.
async fn establish(
    tcp: TcpStream,
    secure: bool,
    we_listened: bool,
    emit: &(dyn Fn(DccEvent) + Send + Sync),
) -> Result<BoxConn, String> {
    if !secure {
        return Ok(Box::new(tcp));
    }
    let (stream, detail) = if we_listened { dcctls::tls_accept(tcp).await? } else { dcctls::tls_connect(tcp).await? };
    emit(DccEvent::Encrypted { detail });
    Ok(stream)
}

// ---------------------------------------------------------------- receiving a file

/// Where a download is written until it is complete: `name.part` next to the destination.
pub fn part_path(final_path: &Path) -> PathBuf {
    let mut s = final_path.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Removes the `.part` file on drop, but only if it is empty. A partial file that holds data is
/// kept after a failure or a cancellation so the download can be resumed; it is discarded only
/// when the user explicitly starts over.
struct EmptyPartCleanup(PathBuf);

impl Drop for EmptyPartCleanup {
    fn drop(&mut self) {
        if std::fs::metadata(&self.0).map(|m| m.len() == 0).unwrap_or(false) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// Receive into `path.part`, then move it over `path` once all `size` bytes are in. With
/// `resume_from > 0` the existing partial file (which must be exactly that long) is continued;
/// acknowledgements always count total bytes, including the part already on disk.
async fn receive_stream(
    stream: BoxConn,
    size: u64,
    path: PathBuf,
    resume_from: u64,
    turbo: bool,
    emit: &(dyn Fn(DccEvent) + Send + Sync),
) -> Result<u64, String> {
    let part = part_path(&path);
    // Declared before `file` so the file handle closes before the cleanup looks at it.
    let _cleanup = EmptyPartCleanup(part.clone());
    let mut file = if resume_from > 0 {
        if resume_from > size {
            return Err("the partial file is already larger than the file being offered".into());
        }
        let mut f = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&part)
            .await
            .map_err(|e| format!("could not reopen the partial file: {e}"))?;
        let len = f.metadata().await.map_err(|e| format!("could not read the partial file: {e}"))?.len();
        if len != resume_from {
            return Err(format!(
                "the partial file has {len} bytes, not the {resume_from} that were agreed; start the download over"
            ));
        }
        f.seek(SeekFrom::End(0)).await.map_err(|e| format!("could not continue the partial file: {e}"))?;
        f
    } else {
        File::create(&part).await.map_err(|e| format!("could not create the file: {e}"))?
    };
    let mut buf = vec![0u8; CHUNK];
    let (mut got, mut last_emit) = (resume_from, Instant::now() - Duration::from_secs(1));
    let mut throttle = Throttle::new();
    // Acknowledgements go out from their own task, newest value only, so a sender that ignores them
    // (or a slow link) can never stall the download. Turbo mode (DCC TSEND / XMIT) sends none.
    let (mut stream, wr) = tokio::io::split(stream);
    let (ack_tx, mut ack_rx) = tokio::sync::watch::channel(0u32);
    let ack_task = if turbo {
        drop(wr);
        None
    } else {
        Some(tokio::spawn(async move {
            let mut wr = wr;
            while ack_rx.changed().await.is_ok() {
                let v = *ack_rx.borrow_and_update();
                if wr.write_all(&v.to_be_bytes()).await.is_err() || wr.flush().await.is_err() {
                    break;
                }
            }
            let _ = wr.shutdown().await; // a clean end (close_notify on TLS)
        }))
    };
    let outcome: Result<(), String> = async {
        while got < size {
            // Never ask for more than the offer declared, so a lying sender cannot make us store extra.
            let want = (size - got).min(throttle.chunk() as u64) as usize;
            let n = match timeout(IDLE_TIMEOUT, stream.read(&mut buf[..want])).await {
                Err(_) => return Err("timed out waiting for data".to_owned()),
                Ok(Err(e)) => return Err(format!("connection error: {e}")),
                Ok(Ok(0)) => return Err(format!("connection closed after {got} of {size} bytes")),
                Ok(Ok(n)) => n,
            };
            file.write_all(&buf[..n]).await.map_err(|e| format!("could not write the file: {e}"))?;
            got += n as u64;
            // Reading more slowly makes TCP slow the sender down too.
            throttle.pace(n as u64).await;
            // Classic DCC acknowledgement: total bytes so far, 32-bit big endian.
            if !turbo {
                let _ = ack_tx.send((got & 0xffff_ffff) as u32);
            }
            if last_emit.elapsed() >= Duration::from_millis(100) {
                last_emit = Instant::now();
                emit(DccEvent::Progress { bytes: got, total: size });
            }
        }
        Ok(())
    }
    .await;
    // Let the last acknowledgement out (a moment at most), then stop the writer.
    drop(ack_tx);
    if let Some(task) = ack_task {
        let handle = task.abort_handle();
        if timeout(Duration::from_secs(2), task).await.is_err() {
            handle.abort();
        }
    }
    // Flush even when the transfer failed: what arrived must really be on disk, because the
    // `.part` file is what a later resume continues from (tokio files buffer writes).
    let flushed = file.flush().await.map_err(|e| format!("could not write the file: {e}"));
    outcome?;
    flushed?;
    drop(file);
    // Only now does the destination change; a failed download never touches an existing file.
    tokio::fs::rename(&part, &path).await.map_err(|e| {
        format!("the download is complete but could not be moved into place ({e}); it is kept as {}", part.display())
    })?;
    emit(DccEvent::Progress { bytes: got, total: size });
    Ok(got)
}

fn finish(result: Result<u64, String>, emit: &(dyn Fn(DccEvent) + Send + Sync)) {
    match result {
        Ok(bytes) => emit(DccEvent::Done { bytes }),
        Err(reason) => emit(DccEvent::Failed { reason }),
    }
}

/// Active receive: connect to the sender at `ip:port` and store exactly `size` bytes in `path`
/// (via `path.part`). `resume_from` is the length of the partial file to continue, or 0.
pub async fn receive_file<E>(
    ip: IpAddr,
    port: u16,
    size: u64,
    path: PathBuf,
    resume_from: u64,
    irc_server: Option<IpAddr>,
    turbo: bool,
    secure: bool,
    emit: E,
) where
    E: Fn(DccEvent) + Send + Sync + 'static,
{
    let result = async {
        let tcp = connect(ip, port, irc_server).await?;
        let stream = establish(tcp, secure, false, &emit).await?;
        receive_stream(stream, size, path, resume_from, turbo, &emit).await
    }
    .await;
    finish(result, &emit);
}

/// Passive (reverse) receive: we already listen; wait for the sender to connect, then receive.
pub async fn receive_file_passive<E>(
    listener: TcpListener,
    wait: Duration,
    size: u64,
    path: PathBuf,
    resume_from: u64,
    turbo: bool,
    secure: bool,
    emit: E,
) where
    E: Fn(DccEvent) + Send + Sync + 'static,
{
    let result = async {
        let tcp = accept_one(listener, wait).await?;
        let stream = establish(tcp, secure, true, &emit).await?;
        receive_stream(stream, size, path, resume_from, turbo, &emit).await
    }
    .await;
    finish(result, &emit);
}

// ---------------------------------------------------------------- sending a file

async fn send_stream(
    stream: BoxConn,
    mut file: File,
    size: u64,
    start: u64,
    turbo: bool,
    emit: &(dyn Fn(DccEvent) + Send + Sync),
) -> Result<u64, String> {
    if start > size {
        return Err("the resume position is past the end of the file".into());
    }
    file.seek(SeekFrom::Start(start)).await.map_err(|e| format!("could not seek in the file: {e}"))?;
    let (mut rd, mut wr) = tokio::io::split(stream);
    // Drain acknowledgements in the background; remember the last one.
    let (ack_tx, mut ack_rx) = tokio::sync::watch::channel(0u32);
    let reader = tokio::spawn(async move {
        let mut b = [0u8; 4];
        loop {
            // acks arrive as 4-byte big-endian counters (possibly split across reads)
            match rd.read_exact(&mut b).await {
                Ok(_) => {
                    let _ = ack_tx.send(u32::from_be_bytes(b));
                }
                Err(_) => break, // EOF or error: the receiver is gone
            }
        }
    });

    let mut buf = vec![0u8; CHUNK];
    let (mut sent, mut last_emit) = (start, Instant::now() - Duration::from_secs(1));
    let mut throttle = Throttle::new();
    while sent < size {
        let want = (size - sent).min(throttle.chunk() as u64) as usize;
        let n = file.read(&mut buf[..want]).await.map_err(|e| format!("could not read the file: {e}"))?;
        if n == 0 {
            reader.abort();
            return Err("the file got shorter while sending".into());
        }
        match timeout(IDLE_TIMEOUT, wr.write_all(&buf[..n])).await {
            Err(_) => return Err("timed out: the receiver stopped reading".into()),
            Ok(Err(e)) => return Err(format!("connection error: {e}")),
            Ok(Ok(())) => {}
        }
        sent += n as u64;
        throttle.pace(n as u64).await;
        if last_emit.elapsed() >= Duration::from_millis(100) {
            last_emit = Instant::now();
            emit(DccEvent::Progress { bytes: sent, total: size });
        }
    }
    let _ = wr.flush().await;
    emit(DccEvent::Progress { bytes: sent, total: size });
    if turbo {
        // No acknowledgements will come: finish the stream cleanly and we are done.
        let _ = wr.shutdown().await;
        reader.abort();
        return Ok(sent);
    }
    // Wait for the final ack, the receiver hanging up, or a short grace period (receivers that
    // send no acks and do not close until we do).
    let want_ack = (size & 0xffff_ffff) as u32;
    let _ = timeout(FINAL_ACK_WAIT, async {
        loop {
            if *ack_rx.borrow() == want_ack || reader.is_finished() {
                break;
            }
            if ack_rx.changed().await.is_err() {
                break;
            }
        }
    })
    .await;
    reader.abort();
    Ok(sent)
}

/// Offer `path` on `listener` (already bound; the caller sent the CTCP offer) and send it to the
/// first peer that connects. `resume` holds the byte to start from: normally 0, raised by the caller
/// if the peer asks to resume (it is read once the connection is accepted, so a `DCC ACCEPT` sent
/// beforehand takes effect).
pub async fn send_file<E>(
    path: PathBuf,
    listener: TcpListener,
    wait: Duration,
    resume: Arc<AtomicU64>,
    turbo: bool,
    secure: bool,
    emit: E,
) where
    E: Fn(DccEvent) + Send + Sync + 'static,
{
    let result = async {
        let file = File::open(&path).await.map_err(|e| format!("could not open the file: {e}"))?;
        let meta = file.metadata().await.map_err(|e| format!("could not read the file: {e}"))?;
        if !meta.is_file() {
            return Err("that is not a file".to_owned());
        }
        let tcp = accept_one(listener, wait).await?;
        let _ = tcp.set_nodelay(true);
        let stream = establish(tcp, secure, true, &emit).await?;
        let start = resume.load(Ordering::SeqCst);
        send_stream(stream, file, meta.len(), start, turbo, &emit).await
    }
    .await;
    finish(result, &emit);
}

/// Passive (reverse) sending: the peer listens and told us where; connect out and send `path`.
/// The connection goes through the same bounce-protection check as any outgoing DCC connection.
pub async fn send_file_to<E>(
    path: PathBuf,
    ip: IpAddr,
    port: u16,
    irc_server: Option<IpAddr>,
    resume: Arc<AtomicU64>,
    turbo: bool,
    secure: bool,
    emit: E,
) where
    E: Fn(DccEvent) + Send + Sync + 'static,
{
    let result = async {
        let file = File::open(&path).await.map_err(|e| format!("could not open the file: {e}"))?;
        let meta = file.metadata().await.map_err(|e| format!("could not read the file: {e}"))?;
        if !meta.is_file() {
            return Err("that is not a file".to_owned());
        }
        let tcp = connect(ip, port, irc_server).await?;
        let _ = tcp.set_nodelay(true);
        let stream = establish(tcp, secure, false, &emit).await?;
        let start = resume.load(Ordering::SeqCst);
        send_stream(stream, file, meta.len(), start, turbo, &emit).await
    }
    .await;
    finish(result, &emit);
}

// ---------------------------------------------------------------- chat

/// Send side of a DCC chat. Dropping it ends the chat.
#[derive(Clone)]
pub struct ChatHandle {
    tx: mpsc::UnboundedSender<String>,
}

impl ChatHandle {
    /// Queue a line (newlines are flattened, long lines cut). False if the chat has ended.
    pub fn send(&self, line: &str) -> bool {
        let mut l: String = line.chars().map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect();
        while l.len() > MAX_CHAT_LINE {
            l.pop();
        }
        self.tx.send(l).is_ok()
    }
}

/// A chat endpoint to hand to [`chat_connect`] / [`chat_accept`].
pub fn chat_channel() -> (ChatHandle, mpsc::UnboundedReceiver<String>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (ChatHandle { tx }, rx)
}

async fn run_chat<E>(stream: BoxConn, mut outgoing: mpsc::UnboundedReceiver<String>, emit: E)
where
    E: Fn(DccEvent) + Send + Sync + 'static,
{
    use tokio::io::{AsyncBufReadExt, BufReader};
    emit(DccEvent::ChatConnected);
    let (rd, mut wr) = tokio::io::split(stream);
    let mut rd = BufReader::new(rd);
    let mut line = Vec::new();
    let reason = loop {
        line.clear();
        tokio::select! {
            read = async {
                // Bounded read: never buffer more than a line's worth from a hostile peer.
                (&mut rd).take(MAX_CHAT_LINE as u64 + 1).read_until(b'\n', &mut line).await
            } => match read {
                Ok(0) => break "the other side closed the chat".to_owned(),
                Ok(_) => {
                    let overlong = line.last() != Some(&b'\n');
                    let mut text = String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_owned();
                    // We read one byte past the cap to detect an over-long line; keep only the cap.
                    while text.len() > MAX_CHAT_LINE {
                        text.pop();
                    }
                    emit(DccEvent::ChatLine(text));
                    if overlong {
                        // discard the rest of an over-long line
                        let mut sink = Vec::new();
                        loop {
                            sink.clear();
                            match (&mut rd).take(4096).read_until(b'\n', &mut sink).await {
                                Ok(0) | Err(_) => break,
                                Ok(_) if sink.last() == Some(&b'\n') => break,
                                Ok(_) => {}
                            }
                        }
                    }
                }
                Err(e) => break format!("connection error: {e}"),
            },
            out = outgoing.recv() => match out {
                Some(text) => {
                    if wr.write_all(format!("{text}\n").as_bytes()).await.is_err() {
                        break "could not send: the connection was lost".to_owned();
                    }
                }
                None => break "you closed the chat".to_owned(),
            },
        }
    };
    emit(DccEvent::ChatClosed { reason });
}

/// Active chat: connect to the peer and run the chat until either side closes.
pub async fn chat_connect<E>(
    ip: IpAddr,
    port: u16,
    irc_server: Option<IpAddr>,
    outgoing: mpsc::UnboundedReceiver<String>,
    secure: bool,
    emit: E,
) where
    E: Fn(DccEvent) + Send + Sync + 'static,
{
    let stream = async {
        let tcp = connect(ip, port, irc_server).await?;
        establish(tcp, secure, false, &emit).await
    }
    .await;
    match stream {
        Ok(stream) => run_chat(stream, outgoing, emit).await,
        Err(reason) => emit(DccEvent::ChatClosed { reason }),
    }
}

/// Listening chat: wait for the peer to connect (one connection), then run the chat.
pub async fn chat_accept<E>(
    listener: TcpListener,
    wait: Duration,
    outgoing: mpsc::UnboundedReceiver<String>,
    secure: bool,
    emit: E,
) where
    E: Fn(DccEvent) + Send + Sync + 'static,
{
    let stream = async {
        let tcp = accept_one(listener, wait).await?;
        establish(tcp, secure, true, &emit).await
    }
    .await;
    match stream {
        Ok(stream) => run_chat(stream, outgoing, emit).await,
        Err(reason) => emit(DccEvent::ChatClosed { reason }),
    }
}

/// Size and readable-file check for an outgoing file, before any offer is made.
pub fn file_size(path: &Path) -> Result<u64, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    Ok(meta.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    fn collect() -> (impl Fn(DccEvent) + Send + Sync + 'static, Arc<Mutex<Vec<DccEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let e2 = events.clone();
        (move |ev| e2.lock().unwrap().push(ev), events)
    }

    fn no_resume() -> Arc<AtomicU64> {
        Arc::new(AtomicU64::new(0))
    }

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("chimerairc-dcc-{}-{tag}", std::process::id()))
    }

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i.wrapping_mul(31).wrapping_add(i >> 8)) as u8).collect()
    }

    async fn wait_for(events: &Arc<Mutex<Vec<DccEvent>>>, done: impl Fn(&DccEvent) -> bool) {
        for _ in 0..400 {
            if events.lock().unwrap().iter().any(&done) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for event; got {:?}", events.lock().unwrap());
    }

    #[tokio::test]
    async fn file_roundtrip_sender_to_receiver() {
        let (src, dst) = (temp("rt-src"), temp("rt-dst"));
        let content = data(3 * 1024 * 1024 + 123);
        std::fs::write(&src, &content).unwrap();

        let (listener, port) = listen(None).await.unwrap();
        let (send_emit, send_ev) = collect();
        let (recv_emit, recv_ev) = collect();
        let sender = tokio::spawn(send_file(src.clone(), listener, Duration::from_secs(5), no_resume(), false, false, send_emit));
        receive_file(LOCAL, port, content.len() as u64, dst.clone(), 0, Some(LOCAL), false, false, recv_emit).await;
        sender.await.unwrap();

        assert_eq!(std::fs::read(&dst).unwrap(), content, "received bytes differ");
        assert!(!part_path(&dst).exists(), "the .part file is renamed away on success");
        let recv = recv_ev.lock().unwrap().clone();
        assert_eq!(recv.last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        let mut last = 0;
        for ev in &recv {
            if let DccEvent::Progress { bytes, total } = ev {
                assert!(*bytes >= last && *bytes <= *total, "progress went backwards or past the total");
                last = *bytes;
            }
        }
        assert_eq!(last, content.len() as u64);
        assert_eq!(send_ev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        let _ = (std::fs::remove_file(src), std::fs::remove_file(dst));
    }

    #[tokio::test]
    async fn empty_files_work() {
        let (src, dst) = (temp("empty-src"), temp("empty-dst"));
        std::fs::write(&src, b"").unwrap();
        let (listener, port) = listen(None).await.unwrap();
        let (se, _) = collect();
        let (re, rev) = collect();
        let s = tokio::spawn(send_file(src.clone(), listener, Duration::from_secs(5), no_resume(), false, false, se));
        receive_file(LOCAL, port, 0, dst.clone(), 0, Some(LOCAL), false, false, re).await;
        s.await.unwrap();
        assert_eq!(std::fs::metadata(&dst).unwrap().len(), 0);
        assert_eq!(rev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: 0 }));
        let _ = (std::fs::remove_file(src), std::fs::remove_file(dst));
    }

    #[tokio::test]
    async fn receiver_never_stores_more_than_declared_and_sends_acks() {
        let dst = temp("lying-dst");
        let (listener, port) = listen(None).await.unwrap();
        let declared = 100_000u64;
        // A sender that lies: sends 50 KB more than it declared, and checks the acks it gets back.
        let liar = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.write_all(&data(declared as usize + 50_000)).await; // may fail once we hang up
            let mut acks = Vec::new();
            let mut b = [0u8; 4];
            while s.read_exact(&mut b).await.is_ok() {
                acks.push(u32::from_be_bytes(b));
            }
            acks
        });
        let (re, rev) = collect();
        receive_file(LOCAL, port, declared, dst.clone(), 0, Some(LOCAL), false, false, re).await;
        assert_eq!(std::fs::metadata(&dst).unwrap().len(), declared, "stored more than the offer declared");
        assert_eq!(rev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: declared }));
        let acks = liar.await.unwrap();
        assert_eq!(acks.last(), Some(&(declared as u32)), "final ack must equal the byte count");
        assert!(acks.windows(2).all(|w| w[0] < w[1]), "acks must increase");
        let _ = std::fs::remove_file(dst);
    }

    #[tokio::test]
    async fn truncated_transfer_fails_but_keeps_the_partial_data_for_resuming() {
        let dst = temp("short-dst");
        let (listener, port) = listen(None).await.unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.write_all(&data(1000)).await;
            // hang up early
        });
        let (re, rev) = collect();
        receive_file(LOCAL, port, 5000, dst.clone(), 0, Some(LOCAL), false, false, re).await;
        let ev = rev.lock().unwrap().clone();
        // Depending on the OS an early hang-up shows up as a clean EOF or as a connection abort.
        assert!(
            matches!(ev.last(), Some(DccEvent::Failed { reason })
                if reason.contains("1000 of 5000") || reason.contains("connection error")),
            "{ev:?}"
        );
        assert!(!dst.exists(), "the destination must not exist until the download is complete");
        let part = part_path(&dst);
        assert_eq!(std::fs::read(&part).unwrap(), data(1000), "what arrived is kept, ready to resume");
        let _ = std::fs::remove_file(part);
    }

    /// A full transfer and a chat over IPv6 loopback (skipped where the machine has no IPv6).
    #[tokio::test]
    async fn file_and_chat_work_over_ipv6() {
        let v6 = IpAddr::V6(std::net::Ipv6Addr::LOCALHOST);
        let Ok((listener, port)) = listen_for(v6, None).await else {
            eprintln!("no IPv6 on this machine; skipping");
            return;
        };
        assert!(listener.local_addr().unwrap().is_ipv6(), "an IPv6 offer needs an IPv6 listener");
        let (src, dst) = (temp("v6-src"), temp("v6-dst"));
        let content = data(300_007);
        std::fs::write(&src, &content).unwrap();
        let (se, sev) = collect();
        let sender = tokio::spawn(send_file(src.clone(), listener, Duration::from_secs(5), no_resume(), false, false, se));
        let (re, rev) = collect();
        // the IRC server is on a local IPv6 address, so a loopback target is allowed
        receive_file(v6, port, content.len() as u64, dst.clone(), 0, Some(v6), false, false, re).await;
        sender.await.unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), content);
        assert_eq!(rev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        assert_eq!(sev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        let _ = (std::fs::remove_file(src), std::fs::remove_file(dst));

        // chat
        let (listener, port) = listen_for(v6, None).await.unwrap();
        let (a_handle, a_rx) = chat_channel();
        let (b_handle, b_rx) = chat_channel();
        let (ae, aev) = collect();
        let (be, bev) = collect();
        let host = tokio::spawn(chat_accept(listener, Duration::from_secs(5), a_rx, false, ae));
        let guest = tokio::spawn(chat_connect(v6, port, Some(v6), b_rx, false, be));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(a_handle.send("hello over v6"));
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(a_handle);
        drop(b_handle);
        let _ = tokio::join!(host, guest);
        assert!(bev.lock().unwrap().iter().any(|e| matches!(e, DccEvent::ChatLine(l) if l == "hello over v6")));
        assert!(aev.lock().unwrap().iter().any(|e| matches!(e, DccEvent::ChatConnected)));
    }

    #[tokio::test]
    async fn an_ipv6_target_is_still_bounce_checked() {
        let v6 = IpAddr::V6(std::net::Ipv6Addr::LOCALHOST);
        let public_server = Some(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)));
        let (re, rev) = collect();
        receive_file(v6, 5000, 10, temp("v6bounce"), 0, public_server, false, false, re).await;
        assert!(matches!(rev.lock().unwrap().last(), Some(DccEvent::Failed { reason }) if reason.contains("refused")));
    }

    /// Passive both ways round: the receiver listens and continues a partial file, the sender
    /// connects out and starts at the agreed byte.
    #[tokio::test]
    async fn passive_resume_completes_a_partial_download() {
        let (src, dst) = (temp("pres-src"), temp("pres-dst"));
        let content = data(500_003);
        std::fs::write(&src, &content).unwrap();
        let have = 123_456u64;
        std::fs::write(part_path(&dst), &content[..have as usize]).unwrap();
        let (listener, port) = listen(None).await.unwrap();
        let (re, rev) = collect();
        let receiver =
            tokio::spawn(receive_file_passive(listener, Duration::from_secs(5), content.len() as u64, dst.clone(), have, false, false, re));
        let (se, _) = collect();
        send_file_to(src.clone(), LOCAL, port, Some(LOCAL), Arc::new(AtomicU64::new(have)), false, false, se).await;
        receiver.await.unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), content);
        assert_eq!(rev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        let _ = (std::fs::remove_file(src), std::fs::remove_file(dst));
    }

    /// Passive sending: the receiver listens, the sender connects out to it.
    #[tokio::test]
    async fn passive_send_connects_out_and_delivers_the_file() {
        let (src, dst) = (temp("psend-src"), temp("psend-dst"));
        let content = data(300_001);
        std::fs::write(&src, &content).unwrap();
        let (listener, port) = listen(None).await.unwrap();
        let (re, rev) = collect();
        let receiver =
            tokio::spawn(receive_file_passive(listener, Duration::from_secs(5), content.len() as u64, dst.clone(), 0, false, false, re));
        let (se, sev) = collect();
        send_file_to(src.clone(), LOCAL, port, Some(LOCAL), no_resume(), false, false, se).await;
        receiver.await.unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), content);
        assert_eq!(sev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        assert_eq!(rev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        let _ = (std::fs::remove_file(src), std::fs::remove_file(dst));
    }

    #[tokio::test]
    async fn passive_send_refuses_a_bounce_target() {
        let src = temp("pbounce-src");
        std::fs::write(&src, data(10)).unwrap();
        let (se, sev) = collect();
        let public_server = Some(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)));
        send_file_to(src.clone(), LOCAL, 6000, public_server, no_resume(), false, false, se).await;
        assert!(matches!(sev.lock().unwrap().last(), Some(DccEvent::Failed { .. })));
        let _ = std::fs::remove_file(src);
    }

    #[test]
    fn pacing_math_keeps_the_average_at_the_limit() {
        let s = Duration::from_secs;
        // 100 KB moved at 100 KB/s must have taken one second
        assert_eq!(pace_delay(100_000, 100_000, Duration::ZERO), s(1));
        assert_eq!(pace_delay(100_000, 100_000, Duration::from_millis(400)), Duration::from_millis(600));
        assert_eq!(pace_delay(100_000, 100_000, s(2)), Duration::ZERO, "already slower than the limit: no wait");
        assert_eq!(pace_delay(0, 1 << 40, Duration::ZERO), Duration::ZERO, "0 = unlimited");
        // chunks shrink with the limit so pauses stay short, but never below 1 KB or above 64 KB
        assert_eq!(chunk_for(0), CHUNK);
        assert_eq!(chunk_for(1), 1024);
        assert_eq!(chunk_for(20_000), 2000);
        assert_eq!(chunk_for(100_000_000), CHUNK);
    }

    /// Resets the global limit even if the test panics, so other tests keep full speed.
    struct LimitGuard;
    impl Drop for LimitGuard {
        fn drop(&mut self) {
            set_rate_limit(0);
        }
    }

    #[tokio::test]
    async fn a_rate_limit_slows_a_real_transfer_and_the_file_is_still_intact() {
        let (src, dst) = (temp("rate-src"), temp("rate-dst"));
        let content = data(1_500_000);
        std::fs::write(&src, &content).unwrap();
        let (listener, port) = listen(None).await.unwrap();
        let _guard = LimitGuard;
        set_rate_limit(1_000_000); // 1 MB/s: 1.5 MB cannot take much less than 1.5 s
        let t0 = Instant::now();
        let (se, _) = collect();
        let sender = tokio::spawn(send_file(src.clone(), listener, Duration::from_secs(10), no_resume(), false, false, se));
        let (re, rev) = collect();
        receive_file(LOCAL, port, content.len() as u64, dst.clone(), 0, Some(LOCAL), false, false, re).await;
        sender.await.unwrap();
        let took = t0.elapsed();
        set_rate_limit(0);
        assert!(took >= Duration::from_millis(1200), "a 1.5 MB transfer at 1 MB/s finished in {took:?}");
        assert!(took < Duration::from_secs(8), "and it should not crawl: {took:?}");
        assert_eq!(std::fs::read(&dst).unwrap(), content);
        assert_eq!(rev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        let _ = (std::fs::remove_file(src), std::fs::remove_file(dst));
    }

    #[tokio::test]
    async fn a_turbo_receiver_sends_no_acknowledgements_but_stores_the_file() {
        let dst = temp("turbo-recv");
        let content = data(200_000);
        let (listener, port) = listen(None).await.unwrap();
        let sent = content.clone();
        let peer = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            s.write_all(&sent).await.unwrap();
            // after the data, anything the receiver wrote back would show up here
            let mut b = [0u8; 4];
            let got = timeout(Duration::from_millis(700), s.read(&mut b)).await;
            matches!(got, Err(_) | Ok(Ok(0))) // silence, or a clean close, but no ack bytes
        });
        let (re, rev) = collect();
        receive_file(LOCAL, port, content.len() as u64, dst.clone(), 0, Some(LOCAL), true, false, re).await;
        assert!(peer.await.unwrap(), "a turbo receiver must not send acknowledgements");
        assert_eq!(std::fs::read(&dst).unwrap(), content);
        assert_eq!(rev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        let _ = std::fs::remove_file(dst);
    }

    #[tokio::test]
    async fn a_turbo_sender_does_not_wait_for_a_final_ack() {
        let src = temp("turbo-send");
        let content = data(100_000);
        std::fs::write(&src, &content).unwrap();
        let (listener, port) = listen(None).await.unwrap();
        // a peer that reads everything but never acks and never hangs up
        let len = content.len();
        let peer = tokio::spawn(async move {
            let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
            let mut got = vec![0u8; len];
            s.read_exact(&mut got).await.unwrap();
            tokio::time::sleep(Duration::from_secs(8)).await;
            got
        });
        let (se, sev) = collect();
        let t0 = Instant::now();
        send_file(src.clone(), listener, Duration::from_secs(5), no_resume(), true, false, se).await;
        assert!(t0.elapsed() < Duration::from_secs(2), "turbo send took {:?}", t0.elapsed());
        assert_eq!(sev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        peer.abort();
        let _ = std::fs::remove_file(src);
    }

    #[tokio::test]
    async fn turbo_both_ways_delivers_the_file_intact() {
        let (src, dst) = (temp("turbo-rt-src"), temp("turbo-rt-dst"));
        let content = data(700_001);
        std::fs::write(&src, &content).unwrap();
        let (listener, port) = listen(None).await.unwrap();
        let (se, sev) = collect();
        let sender = tokio::spawn(send_file(src.clone(), listener, Duration::from_secs(5), no_resume(), true, false, se));
        let (re, rev) = collect();
        receive_file(LOCAL, port, content.len() as u64, dst.clone(), 0, Some(LOCAL), true, false, re).await;
        sender.await.unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), content);
        assert_eq!(sev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        assert_eq!(rev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        let _ = (std::fs::remove_file(src), std::fs::remove_file(dst));
    }

    #[tokio::test]
    async fn secure_dcc_transfers_a_file_over_tls_and_reports_the_encryption() {
        let (src, dst) = (temp("tls-src"), temp("tls-dst"));
        let content = data(900_003);
        std::fs::write(&src, &content).unwrap();
        let (listener, port) = listen(None).await.unwrap();
        let (se, sev) = collect();
        let sender = tokio::spawn(send_file(src.clone(), listener, Duration::from_secs(5), no_resume(), false, true, se));
        let (re, rev) = collect();
        receive_file(LOCAL, port, content.len() as u64, dst.clone(), 0, Some(LOCAL), false, true, re).await;
        sender.await.unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), content);
        for (who, ev) in [("receiver", &rev), ("sender", &sev)] {
            let ev = ev.lock().unwrap().clone();
            assert!(ev.iter().any(|e| matches!(e, DccEvent::Encrypted { detail } if detail.starts_with("TLS"))), "{who}: {ev:?}");
            assert_eq!(ev.last(), Some(&DccEvent::Done { bytes: content.len() as u64 }), "{who}");
        }
        let _ = (std::fs::remove_file(src), std::fs::remove_file(dst));
    }

    #[tokio::test]
    async fn secure_and_turbo_and_passive_combine() {
        let (src, dst) = (temp("tls2-src"), temp("tls2-dst"));
        let content = data(300_001);
        std::fs::write(&src, &content).unwrap();
        // passive: the receiver listens (TLS server), the sender connects out (TLS client), turbo
        let (listener, port) = listen(None).await.unwrap();
        let (re, rev) = collect();
        let receiver =
            tokio::spawn(receive_file_passive(listener, Duration::from_secs(5), content.len() as u64, dst.clone(), 0, true, true, re));
        let (se, _) = collect();
        send_file_to(src.clone(), LOCAL, port, Some(LOCAL), no_resume(), true, true, se).await;
        receiver.await.unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), content);
        assert!(rev.lock().unwrap().iter().any(|e| matches!(e, DccEvent::Encrypted { .. })));
        let _ = (std::fs::remove_file(src), std::fs::remove_file(dst));
    }

    #[tokio::test]
    async fn a_secure_receiver_refuses_a_plain_sender_and_stores_nothing() {
        let dst = temp("tls3-dst");
        let (listener, port) = listen(None).await.unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.write_all(&data(50_000)).await; // plain bytes where a TLS handshake is expected
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let (re, rev) = collect();
        receive_file(LOCAL, port, 50_000, dst.clone(), 0, Some(LOCAL), false, true, re).await;
        assert!(matches!(rev.lock().unwrap().last(), Some(DccEvent::Failed { reason }) if reason.contains("secure")), "{:?}", rev.lock().unwrap());
        assert!(!dst.exists() && !part_path(&dst).exists(), "no file data from an unencrypted peer");
    }

    #[tokio::test]
    async fn secure_chat_works_in_both_directions() {
        let (listener, port) = listen(None).await.unwrap();
        let (a_handle, a_rx) = chat_channel();
        let (b_handle, b_rx) = chat_channel();
        let (ae, aev) = collect();
        let (be, bev) = collect();
        let host = tokio::spawn(chat_accept(listener, Duration::from_secs(5), a_rx, true, ae));
        let guest = tokio::spawn(chat_connect(LOCAL, port, Some(LOCAL), b_rx, true, be));
        wait_for(&bev, |e| matches!(e, DccEvent::ChatConnected)).await;
        assert!(a_handle.send("hello over tls"));
        assert!(b_handle.send("and back again"));
        wait_for(&bev, |e| matches!(e, DccEvent::ChatLine(l) if l == "hello over tls")).await;
        wait_for(&aev, |e| matches!(e, DccEvent::ChatLine(l) if l == "and back again")).await;
        for ev in [&aev, &bev] {
            assert!(ev.lock().unwrap().iter().any(|e| matches!(e, DccEvent::Encrypted { .. })));
        }
        drop(a_handle);
        drop(b_handle);
        let _ = tokio::join!(host, guest);
    }

    #[tokio::test]
    async fn a_hangup_before_any_data_leaves_nothing_behind() {
        let dst = temp("nodata-dst");
        let (listener, port) = listen(None).await.unwrap();
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            drop(s); // accept and hang up at once
        });
        let (re, rev) = collect();
        receive_file(LOCAL, port, 5000, dst.clone(), 0, Some(LOCAL), false, false, re).await;
        assert!(matches!(rev.lock().unwrap().last(), Some(DccEvent::Failed { .. })));
        assert!(!dst.exists() && !part_path(&dst).exists(), "an empty partial file is not worth keeping");
    }

    #[tokio::test]
    async fn cancelling_keeps_the_partial_file_so_the_download_can_be_resumed() {
        let dst = temp("cancel-dst");
        let (listener, port) = listen(None).await.unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.write_all(&data(100)).await;
            tokio::time::sleep(Duration::from_secs(30)).await; // stall with the connection open
        });
        let (re, _) = collect();
        let job = tokio::spawn(receive_file(LOCAL, port, 1_000_000, dst.clone(), 0, Some(LOCAL), false, false, re));
        let part = part_path(&dst);
        for _ in 0..100 {
            if std::fs::metadata(&part).map(|m| m.len() == 100).unwrap_or(false) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0), 100, "the transfer should have started");
        job.abort();
        let _ = job.await;
        assert!(!dst.exists());
        assert_eq!(std::fs::metadata(&part).unwrap().len(), 100, "cancelling must not throw the downloaded data away");
        let _ = std::fs::remove_file(part);
    }

    #[tokio::test]
    async fn a_failed_download_never_touches_an_existing_destination() {
        let dst = temp("keep-dst");
        std::fs::write(&dst, b"precious existing file").unwrap();
        let (listener, port) = listen(None).await.unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.write_all(&data(10)).await;
        });
        let (re, _) = collect();
        receive_file(LOCAL, port, 500, dst.clone(), 0, Some(LOCAL), false, false, re).await;
        assert_eq!(std::fs::read(&dst).unwrap(), b"precious existing file");
        let _ = (std::fs::remove_file(&dst), std::fs::remove_file(part_path(&dst)));
    }

    /// The whole point: the first part is on disk, the sender starts at that byte, the receiver
    /// continues the same file, and the result is identical to the original.
    #[tokio::test]
    async fn resume_completes_a_partial_download_byte_for_byte() {
        let (src, dst) = (temp("res-src"), temp("res-dst"));
        let content = data(1_000_000 + 77);
        std::fs::write(&src, &content).unwrap();
        let have = 400_000u64;
        std::fs::write(part_path(&dst), &content[..have as usize]).unwrap();

        let (listener, port) = listen(None).await.unwrap();
        let resume = Arc::new(AtomicU64::new(have)); // as if a DCC ACCEPT had just been sent
        let (se, sev) = collect();
        let sender = tokio::spawn(send_file(src.clone(), listener, Duration::from_secs(5), resume, false, false, se));
        let (re, rev) = collect();
        receive_file(LOCAL, port, content.len() as u64, dst.clone(), have, Some(LOCAL), false, false, re).await;
        sender.await.unwrap();

        assert_eq!(std::fs::read(&dst).unwrap(), content, "resumed file differs from the original");
        assert!(!part_path(&dst).exists());
        let recv = rev.lock().unwrap().clone();
        assert_eq!(recv.last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        for ev in &recv {
            if let DccEvent::Progress { bytes, total } = ev {
                assert!(*bytes >= have, "progress must start at the resume point, not at 0");
                assert!(*bytes <= *total);
            }
        }
        assert_eq!(sev.lock().unwrap().last(), Some(&DccEvent::Done { bytes: content.len() as u64 }));
        let _ = (std::fs::remove_file(src), std::fs::remove_file(dst));
    }

    #[tokio::test]
    async fn resumed_acks_count_the_bytes_already_on_disk() {
        let dst = temp("resack-dst");
        let content = data(120_000);
        let have = 50_000u64;
        std::fs::write(part_path(&dst), &content[..have as usize]).unwrap();
        let (listener, port) = listen(None).await.unwrap();
        let peer = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.write_all(&content[have as usize..]).await;
            let mut acks = Vec::new();
            let mut b = [0u8; 4];
            while s.read_exact(&mut b).await.is_ok() {
                acks.push(u32::from_be_bytes(b));
            }
            acks
        });
        let (re, _) = collect();
        receive_file(LOCAL, port, 120_000, dst.clone(), have, Some(LOCAL), false, false, re).await;
        let acks = peer.await.unwrap();
        assert!(acks.first().is_some_and(|a| *a as u64 > have), "first ack {:?} should already include the resumed bytes", acks.first());
        assert_eq!(acks.last(), Some(&120_000u32));
        let _ = std::fs::remove_file(dst);
    }

    #[tokio::test]
    async fn resume_refuses_a_partial_file_that_changed_or_is_oversized() {
        let dst = temp("badres-dst");
        // the partial file has 300 bytes but we claim to have 500
        std::fs::write(part_path(&dst), data(300)).unwrap();
        let (listener, port) = listen(None).await.unwrap();
        tokio::spawn(async move {
            let (_s, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let (re, rev) = collect();
        receive_file(LOCAL, port, 1000, dst.clone(), 500, Some(LOCAL), false, false, re).await;
        assert!(matches!(rev.lock().unwrap().last(), Some(DccEvent::Failed { reason }) if reason.contains("300 bytes, not the 500")));
        assert_eq!(std::fs::read(part_path(&dst)).unwrap(), data(300), "the user's partial data must be left alone");

        // a "partial" file already longer than the offer
        let (listener, port) = listen(None).await.unwrap();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        std::fs::write(part_path(&dst), data(900)).unwrap();
        let (re, rev) = collect();
        receive_file(LOCAL, port, 800, dst.clone(), 900, Some(LOCAL), false, false, re).await;
        assert!(matches!(rev.lock().unwrap().last(), Some(DccEvent::Failed { reason }) if reason.contains("larger")));
        // a missing partial file
        let missing = temp("nopart-dst");
        let (listener, port) = listen(None).await.unwrap();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let (re, rev) = collect();
        receive_file(LOCAL, port, 800, missing.clone(), 100, Some(LOCAL), false, false, re).await;
        assert!(matches!(rev.lock().unwrap().last(), Some(DccEvent::Failed { reason }) if reason.contains("reopen")));
        let _ = std::fs::remove_file(part_path(&dst));
    }

    #[tokio::test]
    async fn sender_rejects_a_resume_position_beyond_the_file() {
        let src = temp("past-src");
        std::fs::write(&src, data(1000)).unwrap();
        let (listener, port) = listen(None).await.unwrap();
        let (se, sev) = collect();
        let sender = tokio::spawn(send_file(src.clone(), listener, Duration::from_secs(5), Arc::new(AtomicU64::new(5000)), false, false, se));
        let _peer = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
        sender.await.unwrap();
        assert!(matches!(sev.lock().unwrap().last(), Some(DccEvent::Failed { reason }) if reason.contains("past the end")));
        let _ = std::fs::remove_file(src);
    }

    #[tokio::test]
    async fn bounce_attempts_are_refused_before_any_connection() {
        let dst = temp("bounce-dst");
        // A listener on loopback that must never be contacted when the IRC server is public.
        let (listener, port) = listen(None).await.unwrap();
        let public_server = Some(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)));
        let (re, rev) = collect();
        receive_file(LOCAL, port, 10, dst.clone(), 0, public_server, false, false, re).await;
        let ev = rev.lock().unwrap().clone();
        assert!(matches!(ev.last(), Some(DccEvent::Failed { reason }) if reason.contains("refused")), "{ev:?}");
        assert!(!dst.exists());
        // nobody connected to our listener
        assert!(timeout(Duration::from_millis(150), listener.accept()).await.is_err());
        // and a low port is refused even for a public address
        let (re, rev) = collect();
        receive_file(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 22, 10, dst.clone(), 0, public_server, false, false, re).await;
        assert!(matches!(rev.lock().unwrap().last(), Some(DccEvent::Failed { reason }) if reason.contains("refused")));
    }

    #[tokio::test]
    async fn sender_gives_up_when_nobody_connects() {
        let src = temp("noconn-src");
        std::fs::write(&src, b"hello").unwrap();
        let (listener, _port) = listen(None).await.unwrap();
        let (se, sev) = collect();
        send_file(src.clone(), listener, Duration::from_millis(200), no_resume(), false, false, se).await;
        assert!(matches!(sev.lock().unwrap().last(), Some(DccEvent::Failed { reason }) if reason.contains("timed out")));
        let _ = std::fs::remove_file(src);
        // and a missing file fails cleanly
        let (listener, _p) = listen(None).await.unwrap();
        let (se, sev) = collect();
        send_file(temp("does-not-exist"), listener, Duration::from_millis(200), no_resume(), false, false, se).await;
        assert!(matches!(sev.lock().unwrap().last(), Some(DccEvent::Failed { reason }) if reason.contains("open")));
    }

    #[tokio::test]
    async fn port_range_is_respected() {
        // find two free adjacent-ish ports by binding the range [p, p+3] where p is OS-chosen
        let (probe, p) = listen(None).await.unwrap();
        drop(probe);
        let lo = p.max(1024);
        let (l, port) = listen(Some((lo, lo.saturating_add(3)))).await.unwrap();
        assert!((lo..=lo + 3).contains(&port));
        drop(l);
        // a range below 1024 is ignored (falls back to any port rather than binding privileged ports)
        let (_l, port) = listen(Some((20, 30))).await.unwrap();
        assert!(port >= 1024);
    }

    #[tokio::test]
    async fn chat_exchanges_lines_and_caps_hostile_lines() {
        let (listener, port) = listen(None).await.unwrap();
        let (a_handle, a_rx) = chat_channel();
        let (b_handle, b_rx) = chat_channel();
        let (ea, a_ev) = collect();
        let (eb, b_ev) = collect();
        let a = tokio::spawn(chat_accept(listener, Duration::from_secs(5), a_rx, false, ea));
        let b = tokio::spawn(chat_connect(LOCAL, port, Some(LOCAL), b_rx, false, eb));
        wait_for(&a_ev, |e| *e == DccEvent::ChatConnected).await;
        wait_for(&b_ev, |e| *e == DccEvent::ChatConnected).await;

        assert!(a_handle.send("hello from a"));
        assert!(b_handle.send("two\r\nlines"));
        wait_for(&b_ev, |e| *e == DccEvent::ChatLine("hello from a".into())).await;
        wait_for(&a_ev, |e| *e == DccEvent::ChatLine("two  lines".into())).await;

        // a very long outgoing line is cut to the cap
        assert!(a_handle.send(&"x".repeat(MAX_CHAT_LINE * 3)));
        wait_for(&b_ev, |e| matches!(e, DccEvent::ChatLine(l) if l.len() == MAX_CHAT_LINE)).await;

        drop(a_handle); // ending our side closes the chat for both
        wait_for(&a_ev, |e| matches!(e, DccEvent::ChatClosed { .. })).await;
        wait_for(&b_ev, |e| matches!(e, DccEvent::ChatClosed { .. })).await;
        a.await.unwrap();
        drop(b_handle);
        b.await.unwrap();
    }

    #[tokio::test]
    async fn chat_survives_an_endless_line_from_a_hostile_peer() {
        let (listener, port) = listen(None).await.unwrap();
        let (h, rx) = chat_channel();
        let (e, ev) = collect();
        let chat = tokio::spawn(chat_connect(LOCAL, port, Some(LOCAL), rx, false, e));
        let (mut s, _) = listener.accept().await.unwrap();
        // 1 MB without a newline, then a normal line
        let _ = s.write_all(&vec![b'a'; 1_000_000]).await;
        let _ = s.write_all(b"\nafter\n").await;
        wait_for(&ev, |e| *e == DccEvent::ChatLine("after".into())).await;
        let lines: Vec<String> = ev
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| if let DccEvent::ChatLine(l) = e { Some(l.clone()) } else { None })
            .collect();
        assert!(lines.iter().all(|l| l.len() <= MAX_CHAT_LINE), "an over-long line must be cut, not buffered whole");
        drop(h);
        drop(s);
        chat.await.unwrap();
    }

    #[tokio::test]
    async fn chat_connect_to_a_forbidden_address_is_refused() {
        let (_h, rx) = chat_channel();
        let (e, ev) = collect();
        chat_connect(LOCAL, 5000, Some(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))), rx, false, e).await;
        assert!(matches!(ev.lock().unwrap().last(), Some(DccEvent::ChatClosed { reason }) if reason.contains("refused")));
    }
}
