//! UPnP Internet Gateway Device client: just enough to ask a home router to forward one TCP port
//! to this machine (so a DCC offer can be reached from outside) and to remove it again.
//!
//! Safety rules, all covered by tests:
//! * the router is only trusted if it answers from, and describes itself at, a private,
//!   link-local or loopback IPv4 address, and the address in `LOCATION` must be the sender of the
//!   answer (a LAN device cannot point us at a third host); names are never resolved;
//! * the control URL must live on that same host and port;
//! * every network step has a timeout and every response a size cap; XML with a DTD is refused;
//! * only the one port we are about to listen on is mapped, TCP only, to our own address, with a
//!   lease that expires on its own when the router allows it;
//! * an external address that is not public (double NAT) is treated as a failure.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::timeout;

/// The standard SSDP multicast address and port.
pub fn ssdp_addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 255, 255, 250)), 1900)
}

const MAX_RESPONSE: usize = 256 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(4);
const SEARCH_TARGET: &str = "urn:schemas-upnp-org:device:InternetGatewayDevice:1";

#[derive(Clone, Debug)]
pub struct Gateway {
    addr: SocketAddr,
    control_path: String,
    service: String,
    local_ip: Ipv4Addr,
}

impl Gateway {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    /// Our own address on the network the router is on (what the router should forward to).
    pub fn local_ip(&self) -> Ipv4Addr {
        self.local_ip
    }
}

// ---------------------------------------------------------------- parsing and validation

pub(crate) fn lan_address(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local() || ip.is_loopback()
}

/// Validate a `http://a.b.c.d[:port]/path` URL that a device gave us: plain HTTP, a literal LAN
/// address, and equal to `expect_ip`. Returns the socket address and the path.
fn parse_url(expect_ip: IpAddr, url: &str) -> Result<(SocketAddr, String), String> {
    let rest = url.strip_prefix("http://").ok_or("the router's address is not a plain http:// URL")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_owned()),
        None => (rest, "/".to_owned()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "bad port in the router's address")?),
        None => (authority, 80),
    };
    let ip: Ipv4Addr = host.parse().map_err(|_| "the router's address is not a literal IPv4 address")?;
    if !lan_address(ip) {
        return Err("the router's address is not on a local network".into());
    }
    if IpAddr::V4(ip) != expect_ip {
        return Err("the router's address differs from the host that answered".into());
    }
    if port == 0 || path.len() > 512 || path.chars().any(|c| c.is_control() || c == ' ') {
        return Err("the router's address looks wrong".into());
    }
    Ok((SocketAddr::new(IpAddr::V4(ip), port), path))
}

/// Pull `LOCATION` out of an SSDP answer.
fn ssdp_location(packet: &str) -> Option<&str> {
    packet.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("location").then(|| v.trim())
    })
}

/// Find the WAN connection service in a device description: (service type, control path).
fn parse_description(xml: &str, device: SocketAddr, desc_path: &str) -> Result<(String, String), String> {
    let doc = roxmltree::Document::parse(xml).map_err(|_| "the router's description is not valid XML")?;
    let mut best: Option<(u8, String, String)> = None;
    for svc in doc.descendants().filter(|n| n.has_tag_name("service")) {
        let text = |tag: &str| {
            svc.children().find(|c| c.has_tag_name(tag)).and_then(|c| c.text()).map(|t| t.trim().to_owned())
        };
        let (Some(stype), Some(control)) = (text("serviceType"), text("controlURL")) else { continue };
        let rank = if stype.contains(":WANIPConnection:") {
            0
        } else if stype.contains(":WANPPPConnection:") {
            1
        } else {
            continue;
        };
        if best.as_ref().is_none_or(|(r, _, _)| rank < *r) {
            best = Some((rank, stype, control));
        }
    }
    let (_, stype, control) = best.ok_or("the router has no port-mapping service")?;
    let path = if control.starts_with("http://") {
        let (addr, path) = parse_url(device.ip(), &control)?;
        if addr != device {
            return Err("the router's control URL is on a different host or port".into());
        }
        path
    } else if control.starts_with('/') {
        control
    } else {
        let dir = desc_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        format!("{dir}/{control}")
    };
    if path.len() > 512 || path.chars().any(|c| c.is_control() || c == ' ') {
        return Err("the router's control URL looks wrong".into());
    }
    Ok((stype, path))
}

fn xml_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            '\'' => "&apos;".to_owned(),
            c => c.to_string(),
        })
        .collect()
}

fn soap_body(service: &str, action: &str, args: &[(&str, String)]) -> String {
    let mut inner = String::new();
    for (k, v) in args {
        inner.push_str(&format!("<{k}>{}</{k}>", xml_escape(v)));
    }
    format!(
        "<?xml version=\"1.0\"?>\r\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{service}\">{inner}\
         </u:{action}></s:Body></s:Envelope>"
    )
}

/// The router's own error description from a SOAP fault, if there is one.
fn soap_error(body: &str) -> String {
    let Ok(doc) = roxmltree::Document::parse(body) else { return "the router refused the request".into() };
    let text = |tag: &str| {
        doc.descendants().find(|n| n.has_tag_name(tag)).and_then(|n| n.text()).map(|t| t.trim().to_owned())
    };
    match (text("errorCode"), text("errorDescription")) {
        (Some(c), Some(d)) => format!("the router refused ({c}: {d})"),
        (Some(c), None) => format!("the router refused (error {c})"),
        _ => "the router refused the request".into(),
    }
}

// ---------------------------------------------------------------- tiny HTTP/1.0 client

async fn http(addr: SocketAddr, method: &str, path: &str, headers: &str, body: &str) -> Result<(u16, String), String> {
    let work = async {
        let mut s = TcpStream::connect(addr).await.map_err(|e| format!("could not reach the router: {e}"))?;
        let req = format!(
            "{method} {path} HTTP/1.0\r\nHost: {addr}\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).await.map_err(|e| format!("could not talk to the router: {e}"))?;
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = s.read(&mut buf).await.map_err(|e| format!("could not read from the router: {e}"))?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            if out.len() > MAX_RESPONSE {
                return Err("the router's answer is too large".to_owned());
            }
        }
        let text = String::from_utf8_lossy(&out).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").ok_or("the router's answer is malformed")?;
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse::<u16>().ok())
            .ok_or("the router's answer is malformed")?;
        Ok((status, body.to_owned()))
    };
    timeout(IO_TIMEOUT, work).await.map_err(|_| "the router did not answer in time".to_owned())?
}

pub(crate) fn local_ip_towards(addr: SocketAddr) -> Result<Ipv4Addr, String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    s.connect(addr).map_err(|e| e.to_string())?; // no packet is sent; this only picks the route
    match s.local_addr().map_err(|e| e.to_string())?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Ok(ip),
        _ => Err("could not work out our address on the router's network".into()),
    }
}

// ---------------------------------------------------------------- the protocol

/// Look for a router: send an SSDP search to `search` (normally [`ssdp_addr`]) and take the first
/// answer that validates and describes a port-mapping service.
pub async fn discover(search: SocketAddr, wait: Duration) -> Result<Gateway, String> {
    let sock = UdpSocket::bind("0.0.0.0:0").await.map_err(|e| format!("could not open a UDP socket: {e}"))?;
    let msg = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {SEARCH_TARGET}\r\n\r\n"
    );
    let deadline = Instant::now() + wait;
    let mut last_err = "no router answered".to_owned();
    let mut sent = 0;
    let mut buf = [0u8; 2048];
    while Instant::now() < deadline {
        if sent < 2 {
            let _ = sock.send_to(msg.as_bytes(), search).await;
            sent += 1;
        }
        let slice = (deadline - Instant::now()).min(Duration::from_millis(700));
        let Ok(Ok((n, from))) = timeout(slice, sock.recv_from(&mut buf)).await else { continue };
        let packet = String::from_utf8_lossy(&buf[..n]).into_owned();
        let Some(loc) = ssdp_location(&packet) else { continue };
        let (addr, path) = match parse_url(from.ip(), loc) {
            Ok(v) => v,
            Err(e) => {
                last_err = e;
                continue;
            }
        };
        let (status, body) = match http(addr, "GET", &path, "", "").await {
            Ok(v) => v,
            Err(e) => {
                last_err = e;
                continue;
            }
        };
        if status != 200 {
            last_err = format!("the router's description gave HTTP {status}");
            continue;
        }
        match parse_description(&body, addr, &path) {
            Ok((service, control_path)) => {
                let local_ip = local_ip_towards(addr)?;
                return Ok(Gateway { addr, control_path, service, local_ip });
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

async fn soap(g: &Gateway, action: &str, args: &[(&str, String)]) -> Result<String, String> {
    let headers = format!(
        "Content-Type: text/xml; charset=\"utf-8\"\r\nSOAPAction: \"{}#{action}\"\r\n",
        g.service
    );
    let (status, body) = http(g.addr, "POST", &g.control_path, &headers, &soap_body(&g.service, action, args)).await?;
    if status == 200 {
        Ok(body)
    } else {
        Err(soap_error(&body))
    }
}

/// The router's address on the internet side. Only a public address is accepted.
pub async fn external_ip(g: &Gateway) -> Result<Ipv4Addr, String> {
    let body = soap(g, "GetExternalIPAddress", &[]).await?;
    let doc = roxmltree::Document::parse(&body).map_err(|_| "the router's answer is not valid XML")?;
    let text = doc
        .descendants()
        .find(|n| n.has_tag_name("NewExternalIPAddress"))
        .and_then(|n| n.text())
        .unwrap_or("")
        .trim()
        .to_owned();
    let ip: Ipv4Addr = text.parse().map_err(|_| "the router did not report an external address".to_owned())?;
    if !chimerairc_proto::dcc::is_public(IpAddr::V4(ip)) {
        return Err(format!("the router's external address {ip} is not a public one (is there another router in front of it?)"));
    }
    Ok(ip)
}

/// Forward external TCP `port` to the same port on this machine. Returns the lease actually used
/// (0 = permanent, which the router insisted on; the mapping must then be removed explicitly).
pub async fn add_mapping(g: &Gateway, port: u16, lease_secs: u32) -> Result<u32, String> {
    match add_once(g, port, lease_secs).await {
        Ok(()) => Ok(lease_secs),
        // 725 OnlyPermanentLeasesSupported: some routers refuse timed leases
        Err(e) if lease_secs != 0 && e.contains("725") => add_once(g, port, 0).await.map(|()| 0),
        Err(e) => Err(e),
    }
}

async fn add_once(g: &Gateway, port: u16, lease: u32) -> Result<(), String> {
    let args = [
        ("NewRemoteHost", String::new()),
        ("NewExternalPort", port.to_string()),
        ("NewProtocol", "TCP".into()),
        ("NewInternalPort", port.to_string()),
        ("NewInternalClient", g.local_ip.to_string()),
        ("NewEnabled", "1".into()),
        ("NewPortMappingDescription", "ChimeraIRC DCC".into()),
        ("NewLeaseDuration", lease.to_string()),
    ];
    soap(g, "AddPortMapping", &args).await.map(|_| ())
}

pub async fn delete_mapping(g: &Gateway, port: u16) -> Result<(), String> {
    soap(
        g,
        "DeletePortMapping",
        &[("NewRemoteHost", String::new()), ("NewExternalPort", port.to_string()), ("NewProtocol", "TCP".into())],
    )
    .await
    .map(|_| ())
}

/// A port mapping that has been made; remove it with [`Mapping::close`] once the connection that
/// needed it has been accepted (or the offer has timed out).
#[derive(Clone, Debug)]
pub struct Mapping {
    gateway: Gateway,
    pub port: u16,
    /// The address to put in the DCC offer.
    pub external_ip: Ipv4Addr,
}

impl Mapping {
    pub async fn close(self) -> Result<(), String> {
        delete_mapping(&self.gateway, self.port).await
    }
}

/// Find the router, forward `port`, and learn the external address. On any failure nothing stays
/// mapped.
pub async fn open(search: SocketAddr, port: u16, lease_secs: u32) -> Result<Mapping, String> {
    let gateway = discover(search, Duration::from_secs(3)).await?;
    let external_ip = external_ip(&gateway).await?;
    add_mapping(&gateway, port, lease_secs).await?;
    Ok(Mapping { gateway, port, external_ip })
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    const EXTERNAL: &str = "93.184.216.34";

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Normal,
        /// Only permanent leases (error 725 for a timed one).
        PermanentOnly,
        /// Every AddPortMapping fails with 718 (port already mapped).
        Conflict,
        PrivateExternal,
    }

    struct Fake {
        search: SocketAddr,
        calls: Arc<Mutex<Vec<String>>>,
    }

    fn desc(control: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?><root xmlns=\"urn:schemas-upnp-org:device-1-0\"><device><deviceList><device><deviceList><device>\
             <serviceList>\
             <service><serviceType>urn:schemas-upnp-org:service:Layer3Forwarding:1</serviceType><controlURL>/nope</controlURL></service>\
             <service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType><controlURL>{control}</controlURL></service>\
             </serviceList></device></deviceList></device></deviceList></device></root>"
        )
    }

    /// A pretend router on loopback: answers SSDP searches and serves the description and SOAP.
    async fn fake_router(mode: Mode, control_in_description: String, location_host: Option<String>) -> Fake {
        let http_l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_port = http_l.local_addr().unwrap().port();
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let search = udp.local_addr().unwrap();
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let host = location_host.unwrap_or_else(|| "127.0.0.1".into());
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            while let Ok((_, from)) = udp.recv_from(&mut buf).await {
                let reply = format!(
                    "HTTP/1.1 200 OK
CACHE-CONTROL: max-age=100
Location: http://{host}:{http_port}/desc.xml
ST: x

"
                );
                let _ = udp.send_to(reply.as_bytes(), from).await;
            }
        });
        let c = calls.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = http_l.accept().await else { break };
                let (c, control) = (c.clone(), control_in_description.clone());
                tokio::spawn(async move {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        let n = s.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        req.extend_from_slice(&buf[..n]);
                        let text = String::from_utf8_lossy(&req).into_owned();
                        if let Some((head, body)) = text.split_once("\r\n\r\n") {
                            let len = head
                                .lines()
                                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                                .unwrap_or(0);
                            if body.len() >= len {
                                break;
                            }
                        }
                    }
                    let text = String::from_utf8_lossy(&req).into_owned();
                    let first = text.lines().next().unwrap_or("").to_owned();
                    let resp = if first.starts_with("GET /desc.xml") {
                        let body = desc(&control);
                        format!("HTTP/1.0 200 OK\r\nContent-Type: text/xml\r\n\r\n{body}")
                    } else if first.starts_with("POST /ctl") {
                        let action = text
                            .lines()
                            .find_map(|l| l.strip_prefix("SOAPAction:"))
                            .unwrap_or("")
                            .trim()
                            .trim_matches('"')
                            .rsplit('#')
                            .next()
                            .unwrap_or("")
                            .to_owned();
                        let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
                        c.lock().unwrap().push(format!("{action} {}", body.replace("\r\n", "")));
                        let fault = |code: u32, d: &str| {
                            format!("HTTP/1.0 500 Internal Server Error\r\n\r\n<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"x\"><s:Body><s:Fault><detail><UPnPError><errorCode>{code}</errorCode><errorDescription>{d}</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>")
                        };
                        match action.as_str() {
                            "GetExternalIPAddress" => {
                                let ip = if mode == Mode::PrivateExternal { "10.1.2.3" } else { EXTERNAL };
                                format!("HTTP/1.0 200 OK\r\n\r\n<s:Envelope xmlns:s=\"x\"><s:Body><u:R xmlns:u=\"y\"><NewExternalIPAddress>{ip}</NewExternalIPAddress></u:R></s:Body></s:Envelope>")
                            }
                            "AddPortMapping" if mode == Mode::Conflict => fault(718, "ConflictInMappingEntry"),
                            "AddPortMapping" if mode == Mode::PermanentOnly && !body.contains("<NewLeaseDuration>0<") => {
                                fault(725, "OnlyPermanentLeasesSupported")
                            }
                            _ => "HTTP/1.0 200 OK\r\n\r\n<s:Envelope xmlns:s=\"x\"><s:Body/></s:Envelope>".to_owned(),
                        }
                    } else {
                        "HTTP/1.0 404 Not Found\r\n\r\n".to_owned()
                    };
                    let _ = s.write_all(resp.as_bytes()).await;
                });
            }
        });
        Fake { search, calls }
    }

    #[tokio::test]
    async fn maps_a_port_learns_the_external_address_and_removes_the_mapping() {
        let r = fake_router(Mode::Normal, "/ctl".into(), None).await;
        let m = open(r.search, 5123, 600).await.unwrap();
        assert_eq!(m.external_ip, EXTERNAL.parse::<Ipv4Addr>().unwrap());
        assert_eq!(m.port, 5123);
        m.close().await.unwrap();
        let calls = r.calls.lock().unwrap().clone();
        let actions: Vec<&str> = calls.iter().map(|c| c.split(' ').next().unwrap()).collect();
        assert_eq!(actions, ["GetExternalIPAddress", "AddPortMapping", "DeletePortMapping"]);
        let add = &calls[1];
        for want in [
            "<NewExternalPort>5123</NewExternalPort>",
            "<NewInternalPort>5123</NewInternalPort>",
            "<NewProtocol>TCP</NewProtocol>",
            "<NewLeaseDuration>600</NewLeaseDuration>",
            "<NewInternalClient>127.0.0.1</NewInternalClient>",
            "<NewRemoteHost></NewRemoteHost>",
        ] {
            assert!(add.contains(want), "{want} missing from {add}");
        }
        assert!(calls[2].contains("<NewExternalPort>5123</NewExternalPort>"));
    }

    #[tokio::test]
    async fn a_router_that_only_allows_permanent_leases_is_handled() {
        let r = fake_router(Mode::PermanentOnly, "/ctl".into(), None).await;
        let g = discover(r.search, Duration::from_secs(3)).await.unwrap();
        assert_eq!(add_mapping(&g, 5124, 600).await.unwrap(), 0, "falls back to a permanent lease");
        let adds = r.calls.lock().unwrap().iter().filter(|c| c.starts_with("AddPortMapping")).count();
        assert_eq!(adds, 2, "one refused timed attempt, one permanent");
    }

    #[tokio::test]
    async fn router_errors_are_reported_with_their_reason() {
        let r = fake_router(Mode::Conflict, "/ctl".into(), None).await;
        let e = open(r.search, 5125, 600).await.unwrap_err();
        assert!(e.contains("718") && e.contains("ConflictInMappingEntry"), "{e}");
    }

    #[tokio::test]
    async fn a_private_external_address_means_double_nat_and_is_a_failure() {
        let r = fake_router(Mode::PrivateExternal, "/ctl".into(), None).await;
        let e = open(r.search, 5126, 600).await.unwrap_err();
        assert!(e.contains("not a public one"), "{e}");
        let adds = r.calls.lock().unwrap().iter().filter(|c| c.starts_with("AddPortMapping")).count();
        assert_eq!(adds, 0, "nothing is mapped when the address would be useless");
    }

    #[tokio::test]
    async fn a_control_url_on_another_host_is_refused() {
        let r = fake_router(Mode::Normal, "http://10.9.9.9:80/ctl".into(), None).await;
        let e = discover(r.search, Duration::from_secs(2)).await.unwrap_err();
        assert!(e.contains("different host") || e.contains("differs"), "{e}");
        assert!(r.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_answer_that_points_at_another_host_is_ignored() {
        // the answer comes from 127.0.0.1 but LOCATION names 10.9.9.9
        let r = fake_router(Mode::Normal, "/ctl".into(), Some("10.9.9.9".into())).await;
        let e = discover(r.search, Duration::from_secs(2)).await.unwrap_err();
        assert!(e.contains("differs"), "{e}");
        assert!(r.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn no_router_means_a_clean_timeout() {
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let t0 = Instant::now();
        assert!(discover(silent.local_addr().unwrap(), Duration::from_millis(900)).await.is_err());
        assert!(t0.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn locations_must_be_literal_lan_addresses_that_match_the_sender() {
        let lan: IpAddr = "192.168.1.1".parse().unwrap();
        assert_eq!(parse_url(lan, "http://192.168.1.1:5000/rootDesc.xml").unwrap().1, "/rootDesc.xml");
        assert_eq!(parse_url(lan, "http://192.168.1.1/d.xml").unwrap().0.port(), 80);
        assert!(parse_url(lan, "http://192.168.1.99:5000/d.xml").is_err(), "not the host that answered");
        assert!(parse_url(lan, "https://192.168.1.1/d.xml").is_err(), "plain http only");
        assert!(parse_url(lan, "http://router.local/d.xml").is_err(), "names are never resolved");
        assert!(parse_url(lan, "http://192.168.1.1:0/d.xml").is_err());
        assert!(parse_url(lan, "http://192.168.1.1:99999/d.xml").is_err());
        assert!(parse_url(lan, "http://192.168.1.1/a b").is_err());
        // a public host can never be the router, even if it answers from that address
        let public: IpAddr = "93.184.216.34".parse().unwrap();
        assert!(parse_url(public, "http://93.184.216.34/d.xml").is_err());
        // loopback and link-local are LAN-ish
        assert!(parse_url("127.0.0.1".parse().unwrap(), "http://127.0.0.1:1/d").is_ok());
        assert!(parse_url("169.254.1.1".parse().unwrap(), "http://169.254.1.1/d").is_ok());
    }

    #[test]
    fn descriptions_are_parsed_defensively() {
        let dev: SocketAddr = "192.168.1.1:5000".parse().unwrap();
        let (t, p) = parse_description(&desc("/ctl/IPConn"), dev, "/rootDesc.xml").unwrap();
        assert!(t.ends_with("WANIPConnection:1"));
        assert_eq!(p, "/ctl/IPConn");
        // relative control URLs resolve against the description's directory
        assert_eq!(parse_description(&desc("ctl"), dev, "/igd/desc.xml").unwrap().1, "/igd/ctl");
        // an absolute URL is fine on the same host and port, refused elsewhere
        assert_eq!(parse_description(&desc("http://192.168.1.1:5000/c"), dev, "/d").unwrap().1, "/c");
        assert!(parse_description(&desc("http://192.168.1.1:80/c"), dev, "/d").is_err());
        assert!(parse_description(&desc("http://192.168.1.2:5000/c"), dev, "/d").is_err());
        // garbage, no service, and a DTD (entity bombs) are all errors, not panics
        assert!(parse_description("not xml", dev, "/d").is_err());
        assert!(parse_description("<root/>", dev, "/d").is_err());
        let bomb = "<?xml version=\"1.0\"?><!DOCTYPE r [<!ENTITY a \"aaaa\"><!ENTITY b \"&a;&a;&a;&a;\">]><root>&b;</root>";
        assert!(parse_description(bomb, dev, "/d").is_err());
        // WANIPConnection is preferred over WANPPPConnection
        let both = "<root><service><serviceType>urn:x:service:WANPPPConnection:1</serviceType><controlURL>/ppp</controlURL></service>\
                    <service><serviceType>urn:x:service:WANIPConnection:2</serviceType><controlURL>/ip</controlURL></service></root>";
        assert_eq!(parse_description(both, dev, "/d").unwrap().1, "/ip");
    }

    #[test]
    fn soap_values_are_escaped_and_ssdp_headers_found_in_any_case() {
        let b = soap_body("urn:s:1", "AddPortMapping", &[("NewPortMappingDescription", "a<b>&\"c'".into())]);
        assert!(b.contains("a&lt;b&gt;&amp;&quot;c&apos;"));
        assert!(!b.contains("a<b>"));
        assert_eq!(ssdp_location("HTTP/1.1 200 OK\r\nLOCATION: http://x/y\r\n"), Some("http://x/y"));
        assert_eq!(ssdp_location("HTTP/1.1 200 OK\r\nlocation:http://x/y\r\n"), Some("http://x/y"));
        assert_eq!(ssdp_location("HTTP/1.1 200 OK\r\nST: z\r\n"), None);
    }

    #[tokio::test]
    async fn an_oversized_answer_is_cut_off() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0u8; 1024];
            let _ = s.read(&mut b).await;
            let _ = s.write_all(b"HTTP/1.0 200 OK\r\n\r\n").await;
            let junk = vec![b'x'; 64 * 1024];
            for _ in 0..16 {
                if s.write_all(&junk).await.is_err() {
                    break;
                }
            }
        });
        let e = http(addr, "GET", "/", "", "").await.unwrap_err();
        assert!(e.contains("too large"), "{e}");
    }
}
