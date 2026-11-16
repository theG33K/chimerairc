//! Open one TCP port on the home router so a DCC offer can be reached from outside. Tries the
//! three common ways in turn, newest first: PCP, NAT-PMP (both talk to the default gateway over
//! UDP) and UPnP. The first one that works wins; if none does, the error says what each one said.
//!
//! The protocol modules ([`crate::natpmp`], [`crate::upnp`]) carry the safety rules; on top of
//! them the gateway address given for PCP/NAT-PMP must be on a local network.

use crate::{natpmp, upnp};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Pcp,
    NatPmp,
    Upnp,
}

impl Method {
    pub fn name(self) -> &'static str {
        match self {
            Method::Pcp => "PCP",
            Method::NatPmp => "NAT-PMP",
            Method::Upnp => "UPnP",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Address (port 5351) of the default gateway, for PCP and NAT-PMP; `None` skips both.
    pub gateway: Option<SocketAddr>,
    /// Where to send the UPnP search (normally the SSDP multicast address).
    pub ssdp: SocketAddr,
    /// Retry schedule for PCP and NAT-PMP requests.
    pub waits: Vec<Duration>,
}

impl Options {
    pub fn new(gateway: Option<Ipv4Addr>) -> Options {
        Options {
            gateway: gateway.map(|ip| SocketAddr::new(IpAddr::V4(ip), natpmp::PORT)),
            ssdp: upnp::ssdp_addr(),
            waits: natpmp::default_waits(),
        }
    }
}

#[derive(Clone, Debug)]
enum How {
    Pcp { gw: SocketAddr, client: Ipv4Addr, nonce: [u8; 12] },
    NatPmp { gw: SocketAddr },
    Upnp(upnp::Mapping),
}

/// A port that has been forwarded. Remove it with [`Mapping::close`] when the connection that
/// needed it has arrived or the offer is over.
#[derive(Clone, Debug)]
pub struct Mapping {
    pub method: Method,
    pub port: u16,
    /// The address to put in the DCC offer.
    pub external_ip: Ipv4Addr,
    how: How,
    waits: Vec<Duration>,
}

impl Mapping {
    pub async fn close(self) -> Result<(), String> {
        match self.how {
            How::Pcp { gw, client, nonce } => natpmp::pcp_unmap(gw, client, nonce, self.port, &self.waits).await,
            How::NatPmp { gw } => natpmp::pmp_unmap(gw, self.port, &self.waits).await,
            How::Upnp(m) => m.close().await,
        }
    }
}

/// `label: error`, without repeating the label if the error already starts with it.
fn labelled(method: Method, e: &str) -> String {
    let name = method.name();
    if e.starts_with(name) {
        e.to_owned()
    } else {
        format!("{name}: {e}")
    }
}

/// Forward external TCP `port` to this machine and learn the address to advertise. On failure
/// nothing stays mapped.
pub async fn open(o: &Options, port: u16, lease_secs: u32) -> Result<Mapping, String> {
    let mut problems: Vec<String> = Vec::new();
    if let Some(gw) = o.gateway {
        match (gw.ip(), upnp::local_ip_towards(gw)) {
            (IpAddr::V4(ip), Ok(client)) if upnp::lan_address(ip) => {
                match natpmp::pcp_map(gw, client, port, lease_secs, &o.waits).await {
                    Ok(m) => {
                        return Ok(Mapping {
                            method: Method::Pcp,
                            port,
                            external_ip: m.external_ip,
                            how: How::Pcp { gw, client, nonce: m.nonce.unwrap_or_default() },
                            waits: o.waits.clone(),
                        })
                    }
                    Err(e) => problems.push(labelled(Method::Pcp, &e)),
                }
                match natpmp::pmp_map(gw, port, lease_secs, &o.waits).await {
                    Ok(m) => {
                        return Ok(Mapping {
                            method: Method::NatPmp,
                            port,
                            external_ip: m.external_ip,
                            how: How::NatPmp { gw },
                            waits: o.waits.clone(),
                        })
                    }
                    Err(e) => problems.push(labelled(Method::NatPmp, &e)),
                }
            }
            (IpAddr::V4(_), Err(e)) => problems.push(format!("PCP/NAT-PMP: {e}")),
            _ => problems.push("PCP/NAT-PMP: the gateway is not on a local network".into()),
        }
    }
    match upnp::open(o.ssdp, port, lease_secs).await {
        Ok(m) => Ok(Mapping { method: Method::Upnp, port, external_ip: m.external_ip, how: How::Upnp(m), waits: o.waits.clone() }),
        Err(e) => {
            problems.push(labelled(Method::Upnp, &e));
            Err(problems.join("; "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::natpmp::tests::{gateway, Mode};
    use tokio::net::UdpSocket;

    fn opts(gw: Option<SocketAddr>, ssdp: SocketAddr) -> Options {
        Options { gateway: gw, ssdp, waits: vec![Duration::from_millis(150), Duration::from_millis(250)] }
    }

    /// A UDP port nobody answers on, standing for "no UPnP router here".
    async fn dead_ssdp() -> (UdpSocket, SocketAddr) {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let a = s.local_addr().unwrap();
        (s, a)
    }

    #[tokio::test]
    async fn pcp_is_tried_first_and_removed_on_close() {
        let g = gateway(Mode::Normal).await;
        let (_keep, ssdp) = dead_ssdp().await;
        let m = open(&opts(Some(g.addr), ssdp), 5200, 300).await.unwrap();
        assert_eq!(m.method, Method::Pcp);
        assert_eq!(m.external_ip, Ipv4Addr::new(93, 184, 216, 34));
        m.close().await.unwrap();
        let seen = g.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "one PCP map, one PCP removal, and no NAT-PMP at all");
        assert!(seen.iter().all(|p| p[0] == 2));
    }

    #[tokio::test]
    async fn a_nat_pmp_only_gateway_falls_back_to_nat_pmp() {
        let g = gateway(Mode::PmpOnly).await;
        let (_keep, ssdp) = dead_ssdp().await;
        let m = open(&opts(Some(g.addr), ssdp), 5201, 300).await.unwrap();
        assert_eq!(m.method, Method::NatPmp);
        m.close().await.unwrap();
        let seen = g.seen.lock().unwrap().clone();
        let last = seen.last().unwrap();
        assert_eq!((last[0], last[1]), (0, 2), "the removal is a NAT-PMP map request");
        assert_eq!(&last[8..12], &[0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn when_nothing_works_the_error_says_what_each_method_said() {
        let g = gateway(Mode::Silent).await;
        let (_keep, ssdp) = dead_ssdp().await;
        let e = open(&opts(Some(g.addr), ssdp), 5202, 300).await.unwrap_err();
        for m in ["PCP:", "NAT-PMP:", "UPnP:"] {
            assert!(e.contains(m), "{m} missing from {e}");
        }
    }

    #[tokio::test]
    async fn without_a_known_gateway_only_upnp_is_tried() {
        let (_keep, ssdp) = dead_ssdp().await;
        let e = open(&opts(None, ssdp), 5203, 300).await.unwrap_err();
        assert!(e.starts_with("UPnP:") && !e.contains("PCP") && !e.contains("NAT-PMP"), "{e}");
    }

    #[tokio::test]
    async fn a_gateway_outside_the_local_network_is_never_contacted() {
        let (_keep, ssdp) = dead_ssdp().await;
        let public: SocketAddr = "93.184.216.34:5351".parse().unwrap();
        let e = open(&opts(Some(public), ssdp), 5204, 300).await.unwrap_err();
        assert!(e.contains("not on a local network"), "{e}");
    }

    #[tokio::test]
    async fn a_refusing_router_is_reported_and_leaves_nothing_mapped() {
        let g = gateway(Mode::PrivateExternal).await;
        let (_keep, ssdp) = dead_ssdp().await;
        let e = open(&opts(Some(g.addr), ssdp), 5205, 300).await.unwrap_err();
        assert!(e.contains("not a public one"), "{e}");
    }
}
