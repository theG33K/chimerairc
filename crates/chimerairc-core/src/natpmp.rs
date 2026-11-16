//! NAT-PMP (RFC 6886) and PCP (RFC 6887): ask the default gateway over UDP port 5351 to forward
//! one TCP port to this machine, and to remove it again. Both are simpler and more widely
//! implemented than UPnP on newer routers.
//!
//! Safety rules, all covered by tests:
//! * we only ever talk to the one gateway address we are given, and an answer is accepted only if
//!   it comes from exactly that address and port (anything else on the network is ignored);
//! * every field of an answer is checked: version, opcode, result, the echoed internal port (and,
//!   for PCP, the 96-bit nonce and protocol), lengths are exact;
//! * the external port we are given must be the one we asked for (our offer advertises that very
//!   port), otherwise the mapping is removed again and the attempt fails;
//! * an external address that is not public (double NAT) is a failure and nothing stays mapped;
//! * only TCP, only the one port, only our own address, short lease.

use std::hash::{BuildHasher, Hasher};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

/// The well-known port a gateway listens on for both protocols.
pub const PORT: u16 = 5351;

/// What a gateway granted.
#[derive(Clone, Debug)]
pub struct Mapped {
    pub external_ip: Ipv4Addr,
    pub external_port: u16,
    pub lifetime: u32,
    /// PCP only: the nonce that identifies this mapping when it is removed.
    pub nonce: Option<[u8; 12]>,
}

/// Wait times for the successive sends of one request (the RFCs say 250 ms doubling; we stop
/// sooner because a person is waiting).
pub fn default_waits() -> Vec<Duration> {
    vec![Duration::from_millis(250), Duration::from_millis(500), Duration::from_millis(1000)]
}

/// Send `req` to `gw` and return the first answer that comes from `gw` and that `accept` likes.
async fn exchange(gw: SocketAddr, req: &[u8], waits: &[Duration], accept: &(dyn Fn(&[u8]) -> bool + Send + Sync)) -> Result<Vec<u8>, String> {
    let sock = UdpSocket::bind("0.0.0.0:0").await.map_err(|e| format!("could not open a UDP socket: {e}"))?;
    let mut buf = [0u8; 1100];
    for wait in waits {
        sock.send_to(req, gw).await.map_err(|e| format!("could not reach the gateway: {e}"))?;
        let deadline = Instant::now() + *wait;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Ok(Ok((n, from))) = tokio::time::timeout(left, sock.recv_from(&mut buf)).await else { break };
            if from != gw {
                continue; // not the gateway we asked: never trust it
            }
            if accept(&buf[..n]) {
                return Ok(buf[..n].to_vec());
            }
        }
    }
    Err("the gateway did not answer".into())
}

fn nat_pmp_result(code: u16) -> String {
    match code {
        1 => "unsupported version".into(),
        2 => "not authorized (port mapping is switched off in the router)".into(),
        3 => "network failure".into(),
        4 => "out of resources".into(),
        5 => "unsupported opcode".into(),
        c => format!("error {c}"),
    }
}

fn pcp_result(code: u8) -> String {
    match code {
        1 => "unsupported version".into(),
        2 => "not authorized (port mapping is switched off in the router)".into(),
        3 => "malformed request".into(),
        4 => "unsupported opcode".into(),
        7 => "network failure".into(),
        8 => "out of resources".into(),
        9 => "unsupported protocol".into(),
        10 => "port mapping quota exceeded".into(),
        11 => "the gateway cannot provide an external address".into(),
        12 => "the gateway does not see our address as we do (is there another NAT in between?)".into(),
        c => format!("error {c}"),
    }
}

// ---------------------------------------------------------------- NAT-PMP

/// The gateway's external (public) address. Only a public address is accepted.
pub async fn pmp_external_ip(gw: SocketAddr, waits: &[Duration]) -> Result<Ipv4Addr, String> {
    let ans = exchange(gw, &[0, 0], waits, &|p| p.len() == 12 && p[0] == 0 && p[1] == 128).await?;
    let code = u16::from_be_bytes([ans[2], ans[3]]);
    if code != 0 {
        return Err(format!("NAT-PMP: {}", nat_pmp_result(code)));
    }
    let ip = Ipv4Addr::new(ans[8], ans[9], ans[10], ans[11]);
    check_public(ip)?;
    Ok(ip)
}

fn check_public(ip: Ipv4Addr) -> Result<(), String> {
    if chimerairc_proto::dcc::is_public(IpAddr::V4(ip)) {
        Ok(())
    } else {
        Err(format!("the gateway's external address {ip} is not a public one (is there another router in front of it?)"))
    }
}

async fn pmp_request(gw: SocketAddr, port: u16, lifetime: u32, waits: &[Duration]) -> Result<(u16, u32), String> {
    let mut req = [0u8; 12];
    req[1] = 2; // map TCP
    req[4..6].copy_from_slice(&port.to_be_bytes());
    req[6..8].copy_from_slice(&(if lifetime == 0 { 0 } else { port }).to_be_bytes());
    req[8..12].copy_from_slice(&lifetime.to_be_bytes());
    let ans = exchange(gw, &req, waits, &|p| p.len() == 16 && p[0] == 0 && p[1] == 130 && p[8..10] == port.to_be_bytes()).await?;
    let code = u16::from_be_bytes([ans[2], ans[3]]);
    if code != 0 {
        return Err(format!("NAT-PMP: {}", nat_pmp_result(code)));
    }
    Ok((u16::from_be_bytes([ans[10], ans[11]]), u32::from_be_bytes([ans[12], ans[13], ans[14], ans[15]])))
}

/// Forward external TCP `port` to the same port here. The external address is read first so that a
/// useless (private) one fails before anything is mapped.
pub async fn pmp_map(gw: SocketAddr, port: u16, lease_secs: u32, waits: &[Duration]) -> Result<Mapped, String> {
    let external_ip = pmp_external_ip(gw, waits).await?;
    let (external_port, lifetime) = pmp_request(gw, port, lease_secs.max(1), waits).await?;
    if external_port != port {
        let _ = pmp_unmap(gw, port, waits).await;
        return Err(format!("the gateway offered external port {external_port} instead of {port}"));
    }
    Ok(Mapped { external_ip, external_port, lifetime, nonce: None })
}

pub async fn pmp_unmap(gw: SocketAddr, port: u16, waits: &[Duration]) -> Result<(), String> {
    pmp_request(gw, port, 0, waits).await.map(|_| ())
}

// ---------------------------------------------------------------- PCP

fn random_nonce() -> [u8; 12] {
    let s = std::collections::hash_map::RandomState::new();
    let mut out = [0u8; 12];
    for chunk in out.chunks_mut(4) {
        let mut h = s.build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        chunk.copy_from_slice(&(h.finish() as u32).to_be_bytes());
    }
    out
}

fn pcp_request_bytes(client: Ipv4Addr, nonce: [u8; 12], port: u16, lifetime: u32) -> [u8; 60] {
    let mut r = [0u8; 60];
    r[0] = 2; // version
    r[1] = 1; // request, MAP
    r[4..8].copy_from_slice(&lifetime.to_be_bytes());
    r[8..24].copy_from_slice(&client.to_ipv6_mapped().octets());
    r[24..36].copy_from_slice(&nonce);
    r[36] = 6; // TCP
    r[40..42].copy_from_slice(&port.to_be_bytes());
    r[42..44].copy_from_slice(&port.to_be_bytes()); // suggested external port
    // suggested external address stays all zero: "any"
    r
}

async fn pcp_exchange(
    gw: SocketAddr,
    client: Ipv4Addr,
    nonce: [u8; 12],
    port: u16,
    lifetime: u32,
    waits: &[Duration],
) -> Result<Mapped, String> {
    let req = pcp_request_bytes(client, nonce, port, lifetime);
    let ans = exchange(gw, &req, waits, &|p| {
        // a NAT-PMP-only gateway answers with version 0; accept that here so we can say so quickly
        p.len() >= 4 && (p[0] == 0 || (p.len() == 60 && p[0] == 2 && p[1] == 0x81 && p[24..36] == nonce && p[40..42] == port.to_be_bytes()))
    })
    .await?;
    if ans[0] != 2 {
        return Err("the gateway speaks NAT-PMP, not PCP".into());
    }
    if ans[3] != 0 {
        return Err(format!("PCP: {}", pcp_result(ans[3])));
    }
    if ans[36] != 6 {
        return Err("PCP: the gateway answered for a different protocol".into());
    }
    let ext_v6 = Ipv6Addr::from(<[u8; 16]>::try_from(&ans[44..60]).unwrap());
    let external_ip = ext_v6.to_ipv4_mapped().ok_or("PCP: the gateway's external address is not IPv4")?;
    Ok(Mapped {
        external_ip,
        external_port: u16::from_be_bytes([ans[42], ans[43]]),
        lifetime: u32::from_be_bytes([ans[4], ans[5], ans[6], ans[7]]),
        nonce: Some(nonce),
    })
}

/// Forward external TCP `port` to the same port at `client` (our address on the gateway's network).
pub async fn pcp_map(gw: SocketAddr, client: Ipv4Addr, port: u16, lease_secs: u32, waits: &[Duration]) -> Result<Mapped, String> {
    let nonce = random_nonce();
    let m = pcp_exchange(gw, client, nonce, port, lease_secs.max(1), waits).await?;
    if m.external_port != port {
        let _ = pcp_unmap(gw, client, nonce, port, waits).await;
        return Err(format!("the gateway offered external port {} instead of {port}", m.external_port));
    }
    if let Err(e) = check_public(m.external_ip) {
        let _ = pcp_unmap(gw, client, nonce, port, waits).await;
        return Err(e);
    }
    Ok(m)
}

pub async fn pcp_unmap(gw: SocketAddr, client: Ipv4Addr, nonce: [u8; 12], port: u16, waits: &[Duration]) -> Result<(), String> {
    pcp_exchange(gw, client, nonce, port, 0, waits).await.map(|_| ())
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const EXT: Ipv4Addr = Ipv4Addr::new(93, 184, 216, 34);
    const CLIENT: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 20);

    fn fast() -> Vec<Duration> {
        vec![Duration::from_millis(150), Duration::from_millis(250)]
    }

    #[derive(Clone, Copy, PartialEq)]
    pub(crate) enum Mode {
        Normal,
        /// Refuse mapping requests with this result code.
        Refuse(u16),
        /// Grant a different external port than the one asked for.
        OtherPort,
        PrivateExternal,
        /// Only speaks NAT-PMP (answers PCP packets with a version 0 error).
        PmpOnly,
        /// Sends a forged answer from another socket first.
        Spoofer,
        Silent,
    }

    pub(crate) struct Fake {
        pub(crate) addr: SocketAddr,
        pub(crate) seen: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    pub(crate) async fn gateway(mode: Mode) -> Fake {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let log = seen.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1100];
            while let Ok((n, from)) = sock.recv_from(&mut buf).await {
                let p = buf[..n].to_vec();
                log.lock().unwrap().push(p.clone());
                if mode == Mode::Silent {
                    continue;
                }
                if mode == Mode::Spoofer {
                    // a bogus "success" from a different port, which must be ignored
                    let spoof = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                    let mut fake = vec![0u8; 12];
                    fake[1] = 128;
                    fake[8..12].copy_from_slice(&[6, 6, 6, 6]);
                    let _ = spoof.send_to(&fake, from).await;
                }
                let reply: Vec<u8> = if p[0] == 2 {
                    // PCP
                    if mode == Mode::PmpOnly {
                        vec![0, 129, 0, 1, 0, 0, 0, 0] // version 0, result UNSUPP_VERSION
                    } else {
                        let mut r = vec![0u8; 60];
                        r[0] = 2;
                        r[1] = 0x81;
                        if let Mode::Refuse(c) = mode {
                            r[3] = c as u8;
                        }
                        r[4..8].copy_from_slice(&p[4..8]); // lifetime echoed
                        r[24..44].copy_from_slice(&p[24..44]); // nonce, protocol, reserved, internal port
                        let port = if mode == Mode::OtherPort { 40000u16 } else { u16::from_be_bytes([p[40], p[41]]) };
                        r[42..44].copy_from_slice(&port.to_be_bytes());
                        let ext = if mode == Mode::PrivateExternal { Ipv4Addr::new(10, 1, 1, 1) } else { EXT };
                        r[44..60].copy_from_slice(&ext.to_ipv6_mapped().octets());
                        r
                    }
                } else if p[1] == 0 {
                    let mut r = vec![0u8; 12];
                    r[1] = 128;
                    let ext = if mode == Mode::PrivateExternal { Ipv4Addr::new(10, 1, 1, 1) } else { EXT };
                    r[8..12].copy_from_slice(&ext.octets());
                    r
                } else {
                    let mut r = vec![0u8; 16];
                    r[1] = 130;
                    if let Mode::Refuse(c) = mode {
                        r[2..4].copy_from_slice(&c.to_be_bytes());
                    }
                    r[8..10].copy_from_slice(&p[4..6]); // internal port
                    let want = if mode == Mode::OtherPort && p[8..12] != [0, 0, 0, 0] { 40000u16 } else { u16::from_be_bytes([p[6], p[7]]) };
                    r[10..12].copy_from_slice(&want.to_be_bytes());
                    r[12..16].copy_from_slice(&p[8..12]);
                    r
                };
                let _ = sock.send_to(&reply, from).await;
            }
        });
        Fake { addr, seen }
    }

    #[tokio::test]
    async fn nat_pmp_maps_a_port_and_removes_it() {
        let g = gateway(Mode::Normal).await;
        let m = pmp_map(g.addr, 5123, 300, &fast()).await.unwrap();
        assert_eq!((m.external_ip, m.external_port, m.lifetime), (EXT, 5123, 300));
        pmp_unmap(g.addr, 5123, &fast()).await.unwrap();
        let seen = g.seen.lock().unwrap().clone();
        assert_eq!(seen[0], [0, 0], "first the external address is asked for");
        let map = &seen[1];
        assert_eq!((map[0], map[1]), (0, 2), "version 0, map TCP");
        assert_eq!(&map[4..6], &5123u16.to_be_bytes());
        assert_eq!(&map[6..8], &5123u16.to_be_bytes(), "suggested external port = the same port");
        assert_eq!(&map[8..12], &300u32.to_be_bytes());
        let del = seen.last().unwrap();
        assert_eq!(&del[6..8], &[0, 0], "removal: external port 0");
        assert_eq!(&del[8..12], &[0, 0, 0, 0], "removal: lifetime 0");
    }

    #[tokio::test]
    async fn pcp_maps_a_port_with_a_nonce_and_removes_it_with_the_same_one() {
        let g = gateway(Mode::Normal).await;
        let m = pcp_map(g.addr, CLIENT, 5124, 300, &fast()).await.unwrap();
        assert_eq!((m.external_ip, m.external_port), (EXT, 5124));
        let nonce = m.nonce.unwrap();
        pcp_unmap(g.addr, CLIENT, nonce, 5124, &fast()).await.unwrap();
        let seen = g.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        let (add, del) = (&seen[0], &seen[1]);
        assert_eq!((add[0], add[1], add.len()), (2, 1, 60), "PCP v2, MAP, 60 bytes");
        assert_eq!(&add[8..24], &CLIENT.to_ipv6_mapped().octets(), "our address as an IPv4-mapped IPv6 address");
        assert_eq!(&add[24..36], &nonce);
        assert_eq!((add[36], &add[40..42]), (6, &5124u16.to_be_bytes()[..]), "TCP, internal port");
        assert_eq!(&add[4..8], &300u32.to_be_bytes());
        assert_eq!(&del[24..36], &nonce, "removal uses the same nonce");
        assert_eq!(&del[4..8], &[0, 0, 0, 0], "removal: lifetime 0");
    }

    #[tokio::test]
    async fn refusals_are_reported_with_a_reason() {
        let g = gateway(Mode::Refuse(2)).await;
        let e = pmp_map(g.addr, 5125, 300, &fast()).await.unwrap_err();
        assert!(e.contains("not authorized"), "{e}");
        let e = pcp_map(g.addr, CLIENT, 5125, 300, &fast()).await.unwrap_err();
        assert!(e.contains("not authorized"), "{e}");
    }

    #[tokio::test]
    async fn a_different_external_port_is_refused_and_removed_again() {
        let g = gateway(Mode::OtherPort).await;
        let e = pmp_map(g.addr, 5126, 300, &fast()).await.unwrap_err();
        assert!(e.contains("40000"), "{e}");
        let e = pcp_map(g.addr, CLIENT, 5126, 300, &fast()).await.unwrap_err();
        assert!(e.contains("40000"), "{e}");
        let seen = g.seen.lock().unwrap().clone();
        let removals = seen.iter().filter(|p| (p[0] == 2 && p[4..8] == [0, 0, 0, 0]) || (p[0] == 0 && p[1] == 2 && p[8..12] == [0, 0, 0, 0])).count();
        assert_eq!(removals, 2, "each attempt cleans up after itself");
    }

    #[tokio::test]
    async fn a_private_external_address_is_double_nat_and_nothing_stays_mapped() {
        let g = gateway(Mode::PrivateExternal).await;
        let e = pmp_map(g.addr, 5127, 300, &fast()).await.unwrap_err();
        assert!(e.contains("not a public one"), "{e}");
        let seen = g.seen.lock().unwrap().clone();
        assert!(seen.iter().all(|p| !(p[0] == 0 && p[1] == 2)), "NAT-PMP asks for the address first and maps nothing");
        let e = pcp_map(g.addr, CLIENT, 5127, 300, &fast()).await.unwrap_err();
        assert!(e.contains("not a public one"), "{e}");
        let seen = g.seen.lock().unwrap().clone();
        let last = seen.last().unwrap();
        assert_eq!((last[0], &last[4..8]), (2, &[0u8, 0, 0, 0][..]), "PCP removes what it just added");
    }

    #[tokio::test]
    async fn a_pmp_only_gateway_is_recognised_when_pcp_is_tried() {
        let g = gateway(Mode::PmpOnly).await;
        let e = pcp_map(g.addr, CLIENT, 5128, 300, &fast()).await.unwrap_err();
        assert!(e.contains("NAT-PMP"), "{e}");
        assert!(pmp_map(g.addr, 5128, 300, &fast()).await.is_ok(), "and NAT-PMP works on it");
    }

    #[tokio::test]
    async fn answers_from_anyone_but_the_gateway_are_ignored() {
        let g = gateway(Mode::Spoofer).await;
        let ip = pmp_external_ip(g.addr, &fast()).await.unwrap();
        assert_eq!(ip, EXT, "the forged 6.6.6.6 answer must be ignored");
    }

    #[tokio::test]
    async fn a_silent_gateway_times_out_cleanly() {
        let g = gateway(Mode::Silent).await;
        let t0 = Instant::now();
        assert!(pmp_map(g.addr, 5129, 300, &fast()).await.is_err());
        assert!(pcp_map(g.addr, CLIENT, 5129, 300, &fast()).await.is_err());
        assert!(t0.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn malformed_answers_are_rejected() {
        // a gateway that answers every packet with junk of the wrong shape
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        tokio::spawn(async move {
            let mut b = [0u8; 100];
            while let Ok((_, from)) = sock.recv_from(&mut b).await {
                let _ = sock.send_to(&[0, 128, 0, 0, 1, 2, 3], from).await; // too short
                let _ = sock.send_to(&[9; 12], from).await; // wrong version/opcode
            }
        });
        assert!(pmp_external_ip(addr, &fast()).await.is_err());
    }

    #[test]
    fn nonces_differ_between_requests() {
        assert_ne!(random_nonce(), random_nonce());
    }
}
