//! DCC (Direct Client-to-Client) offers carried in CTCP: `DCC SEND`, `DCC CHAT`.
//!
//! Pure parsing / encoding plus the safety rules that must not live in the UI:
//! * [`safe_filename`] turns a name chosen by a stranger into one safe to create on Windows.
//! * [`target_allowed`] refuses to connect to addresses an offer should never point us at
//!   (the classic DCC "bounce" attack: your own machine, your router, cloud metadata, low ports).

use crate::Ctcp;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// A parsed `DCC` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dcc {
    /// `DCC SEND <file> <ip> <port> <size> [token]`. `port == 0` with a token is a *passive*
    /// (reverse) offer: we are expected to listen and reply with our own address.
    ///
    /// `turbo` is set for `DCC TSEND` (mIRC's turbo mode) and for `DCC XMIT clear` (the CTCP
    /// draft's stream transfer): the receiver sends no acknowledgements and the sender does not
    /// wait for them. Everything else is the same as `SEND`.
    ///
    /// `secure` is set for `DCC SSEND` (mIRC, HexChat, irssi): the same transfer, run inside TLS,
    /// with the side that listens acting as the TLS server.
    Send { filename: String, ip: IpAddr, port: u16, size: u64, token: Option<String>, turbo: bool, secure: bool },
    /// `DCC CHAT chat <ip> <port> [token]`; `secure` for `DCC SCHAT` (the chat runs inside TLS).
    Chat { ip: IpAddr, port: u16, token: Option<String>, secure: bool },
    /// `DCC RESUME <file> <port> <position> [token]`: the receiver already has `position` bytes
    /// and asks the sender to continue from there.
    Resume { filename: String, port: u16, position: u64, token: Option<String> },
    /// `DCC ACCEPT <file> <port> <position> [token]`: the sender agrees to start at `position`.
    Accept { filename: String, port: u16, position: u64, token: Option<String> },
}

impl Dcc {
    /// A passive (reverse) *offer*: port 0 plus a token. Resume/Accept are never offers.
    pub fn is_passive(&self) -> bool {
        match self {
            Dcc::Send { port, token, .. } | Dcc::Chat { port, token, .. } => *port == 0 && token.is_some(),
            Dcc::Resume { .. } | Dcc::Accept { .. } => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    NotDcc,
    /// A DCC subcommand we do not handle (XSEND, SSEND, XMIT with another protocol, ...).
    Unsupported,
    Malformed,
}

/// Split on spaces, keeping `"quoted strings"` (which may contain spaces) as one token.
fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut in_quotes, mut have) = (false, false);
    for c in s.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                have = true;
            }
            ' ' if !in_quotes => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            c => {
                cur.push(c);
                have = true;
            }
        }
    }
    if have {
        out.push(cur);
    }
    out
}

/// DCC addresses are usually a decimal 32-bit integer (network order); some clients send text.
pub fn parse_ip(s: &str) -> Option<IpAddr> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse::<u32>().ok().map(|n| IpAddr::V4(Ipv4Addr::from(n)));
    }
    s.parse::<IpAddr>().ok()
}

/// Parse a CTCP into a DCC request.
pub fn parse(c: &Ctcp) -> Result<Dcc, ParseError> {
    if c.command != "DCC" {
        return Err(ParseError::NotDcc);
    }
    let t = tokens(&c.args);
    let kind = t.first().map(|s| s.to_ascii_uppercase()).ok_or(ParseError::Malformed)?;
    match kind.as_str() {
        "SEND" | "TSEND" | "SSEND" => {
            // SEND|TSEND|SSEND <file> <ip> <port> <size> [token]
            let [_, file, ip, port, size, rest @ ..] = t.as_slice() else { return Err(ParseError::Malformed) };
            Ok(Dcc::Send {
                filename: file.clone(),
                ip: parse_ip(ip).ok_or(ParseError::Malformed)?,
                port: port.parse().map_err(|_| ParseError::Malformed)?,
                size: size.parse().map_err(|_| ParseError::Malformed)?,
                token: rest.first().cloned(),
                turbo: kind == "TSEND",
                secure: kind == "SSEND",
            })
        }
        "XMIT" => {
            // XMIT <protocol> <ip> <port> [<name> [<size> [<mime>]]]  (the CTCP draft, section 4.7.2)
            let [_, protocol, ip, port, rest @ ..] = t.as_slice() else { return Err(ParseError::Malformed) };
            if !protocol.eq_ignore_ascii_case("clear") {
                return Err(ParseError::Unsupported); // encrypted or unknown stream protocols
            }
            // A file we can store needs a real name and size; '-' is the draft's "unknown" marker
            // (live feeds, streams of unknown length), which we do not accept.
            let (Some(name), Some(size)) = (rest.first(), rest.get(1)) else { return Err(ParseError::Unsupported) };
            if name == "-" || size == "-" {
                return Err(ParseError::Unsupported);
            }
            Ok(Dcc::Send {
                filename: name.clone(),
                ip: parse_ip(ip).ok_or(ParseError::Malformed)?,
                port: port.parse().map_err(|_| ParseError::Malformed)?,
                size: size.parse().map_err(|_| ParseError::Malformed)?,
                token: None,
                turbo: true,
                secure: false,
            })
        }
        "CHAT" | "SCHAT" => {
            // CHAT|SCHAT <protocol> <ip> <port> [token]
            let [_, _proto, ip, port, rest @ ..] = t.as_slice() else { return Err(ParseError::Malformed) };
            Ok(Dcc::Chat {
                ip: parse_ip(ip).ok_or(ParseError::Malformed)?,
                port: port.parse().map_err(|_| ParseError::Malformed)?,
                token: rest.first().cloned(),
                secure: kind == "SCHAT",
            })
        }
        "RESUME" | "ACCEPT" => {
            // RESUME|ACCEPT <file> <port> <position> [token]
            let [_, file, port, pos, rest @ ..] = t.as_slice() else { return Err(ParseError::Malformed) };
            let (filename, token) = (file.clone(), rest.first().cloned());
            let port = port.parse().map_err(|_| ParseError::Malformed)?;
            let position = pos.parse().map_err(|_| ParseError::Malformed)?;
            Ok(if kind == "RESUME" {
                Dcc::Resume { filename, port, position, token }
            } else {
                Dcc::Accept { filename, port, position, token }
            })
        }
        _ => Err(ParseError::Unsupported),
    }
}

/// The CTCP a receiver sends to ask the sender to continue a partial file from `position`.
pub fn resume_request(filename: &str, port: u16, position: u64, token: Option<&str>) -> Ctcp {
    let token = token.map(|t| format!(" {t}")).unwrap_or_default();
    Ctcp { command: "DCC".into(), args: format!("RESUME {} {port} {position}{token}", quote_if_needed(filename)) }
}

/// The CTCP a sender answers a resume request with.
pub fn accept_reply(filename: &str, port: u16, position: u64, token: Option<&str>) -> Ctcp {
    let token = token.map(|t| format!(" {t}")).unwrap_or_default();
    Ctcp { command: "DCC".into(), args: format!("ACCEPT {} {port} {position}{token}", quote_if_needed(filename)) }
}

fn encode_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => u32::from(v4).to_string(),
        IpAddr::V6(v6) => v6.to_string(),
    }
}

fn quote_if_needed(name: &str) -> String {
    if name.contains(' ') {
        format!("\"{}\"", name.replace('"', ""))
    } else {
        name.to_owned()
    }
}

/// The CTCP to offer a file: `DCC SEND "name" <ip> <port> <size> [token]`. Pass the peer's token
/// back when answering a passive (reverse) offer.
pub fn send_offer(filename: &str, ip: IpAddr, port: u16, size: u64, token: Option<&str>) -> Ctcp {
    send_offer_kind(false, false, filename, ip, port, size, token)
}

/// Like [`send_offer`], using `DCC TSEND` (turbo: no acknowledgements) when `turbo` is set, or
/// `DCC SSEND` (TLS) when `secure` is set. An answer to a passive offer must use the same verb as
/// the offer.
pub fn send_offer_kind(turbo: bool, secure: bool, filename: &str, ip: IpAddr, port: u16, size: u64, token: Option<&str>) -> Ctcp {
    let token = token.map(|t| format!(" {t}")).unwrap_or_default();
    // there is no combined verb: a secure offer is `SSEND`
    let verb = if secure {
        "SSEND"
    } else if turbo {
        "TSEND"
    } else {
        "SEND"
    };
    Ctcp {
        command: "DCC".into(),
        args: format!("{verb} {} {} {port} {size}{token}", quote_if_needed(filename), encode_ip(ip)),
    }
}

/// The CTCP to offer a chat: `DCC CHAT chat <ip> <port>`.
pub fn chat_offer(ip: IpAddr, port: u16) -> Ctcp {
    chat_offer_kind(false, ip, port)
}

/// `DCC SCHAT chat <ip> <port>` when `secure`.
pub fn chat_offer_kind(secure: bool, ip: IpAddr, port: u16) -> Ctcp {
    let verb = if secure { "SCHAT" } else { "CHAT" };
    Ctcp { command: "DCC".into(), args: format!("{verb} chat {} {port}", encode_ip(ip)) }
}

/// Answer a passive chat offer (or make one): `DCC CHAT chat <ip> <port> <token>`.
pub fn chat_offer_token(ip: IpAddr, port: u16, token: &str) -> Ctcp {
    chat_offer_token_kind(false, ip, port, token)
}

pub fn chat_offer_token_kind(secure: bool, ip: IpAddr, port: u16, token: &str) -> Ctcp {
    let verb = if secure { "SCHAT" } else { "CHAT" };
    Ctcp { command: "DCC".into(), args: format!("{verb} chat {} {port} {token}", encode_ip(ip)) }
}

/// A passive (reverse) file offer: we cannot be reached, so address and port are 0 and the peer
/// is asked to listen and answer with the same token.
pub fn passive_send_offer(filename: &str, size: u64, token: &str) -> Ctcp {
    passive_send_offer_kind(false, false, filename, size, token)
}

pub fn passive_send_offer_kind(turbo: bool, secure: bool, filename: &str, size: u64, token: &str) -> Ctcp {
    send_offer_kind(turbo, secure, filename, IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0, size, Some(token))
}

/// A passive chat offer: `DCC CHAT chat 0 0 <token>`.
pub fn passive_chat_offer(token: &str) -> Ctcp {
    passive_chat_offer_kind(false, token)
}

pub fn passive_chat_offer_kind(secure: bool, token: &str) -> Ctcp {
    chat_offer_token_kind(secure, IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0, token)
}

// ---------------------------------------------------------------- filenames

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
    "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Extensions Windows will happily run (or that run code inside another program) when opened.
const RISKY: &[&str] = &[
    "exe", "scr", "com", "pif", "bat", "cmd", "msi", "msp", "lnk", "vbs", "vbe", "js", "jse", "wsf", "wsh", "ps1",
    "psm1", "hta", "cpl", "jar", "reg", "dll", "sys", "iso", "img", "docm", "xlsm", "pptm",
];

/// A filename chosen by a stranger, made safe to create locally: directory parts dropped,
/// no invalid characters, no reserved device names, no trailing dots or spaces, at most 120 chars
/// (the extension is kept). Never empty.
pub fn safe_filename(name: &str) -> String {
    // Only the last path component, whichever separator the sender used.
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String =
        base.chars().map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c }).collect();
    while s.ends_with(['.', ' ']) {
        s.pop();
    }
    let lead = s.chars().take_while(|c| *c == '.' || *c == ' ').count();
    s = s.chars().enumerate().map(|(i, c)| if i < lead { '_' } else { c }).collect();
    if s.is_empty() {
        return "download".into();
    }
    if s.chars().count() > 120 {
        let ext_start = s.rfind('.').filter(|i| s.len() - i <= 12);
        let ext: String = ext_start.map(|i| s[i..].to_owned()).unwrap_or_default();
        let keep = 120 - ext.chars().count();
        s = s.chars().take(keep).collect::<String>() + &ext;
    }
    let stem = s.split('.').next().unwrap_or("").to_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        s.insert(0, '_');
    }
    s
}

/// Does opening this file run code? (Used to show a stronger warning, never to block.)
pub fn is_risky_extension(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, e)| RISKY.contains(&e.to_ascii_lowercase().as_str()))
}

// ---------------------------------------------------------------- bounce protection

/// Ordinary public internet address (not loopback, private, link-local, CGNAT, multicast, ...).
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_unspecified()
                || v.is_loopback()
                || v.is_private()
                || v.is_link_local()
                || v.is_broadcast()
                || v.is_multicast()
                || v.is_documentation()
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 carrier-grade NAT
                || o[0] == 0
                || o[0] >= 240) // reserved
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let seg = v.segments();
            !(v.is_unspecified()
                || v.is_loopback()
                || v.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (seg[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (seg[0] == 0x2001 && seg[1] == 0x0db8) // documentation 2001:db8::/32
                || (seg[0] == 0x0100 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0) // discard-only 100::/64
                || v == Ipv6Addr::LOCALHOST)
        }
    }
}

/// May we connect to the address a DCC offer asks for?
///
/// * ports below 1024 are never allowed (they belong to services like SSH, SMTP, HTTP);
/// * unspecified / multicast / broadcast addresses are never allowed;
/// * non-public addresses (loopback, LAN, link-local, ...) are allowed only when the IRC server
///   itself is on such a network, i.e. a genuine LAN or local setup. Otherwise an offer could aim
///   us at your own machine or router.
pub fn target_allowed(ip: IpAddr, port: u16, irc_server: Option<IpAddr>) -> Result<(), &'static str> {
    if port < 1024 {
        return Err("the offer uses a port below 1024");
    }
    let unusable = match ip {
        IpAddr::V4(v) => v.is_unspecified() || v.is_multicast() || v.is_broadcast(),
        IpAddr::V6(v) => v.is_unspecified() || v.is_multicast(),
    };
    if unusable {
        return Err("the offer uses an unusable address");
    }
    if !is_public(ip) && irc_server.is_none_or(is_public) {
        return Err("the offer points at a private or local address, which a stranger could use to probe your network");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(args: &str) -> Ctcp {
        Ctcp { command: "DCC".into(), args: args.into() }
    }

    fn v4(a: u8, b: u8, cc: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, cc, d))
    }

    #[test]
    fn parses_send_with_integer_ip_and_quoted_names() {
        // 3232235777 = 192.168.1.1
        let d = parse(&c("SEND \"my file.zip\" 3232235777 5000 123456")).unwrap();
        assert_eq!(
            d,
            Dcc::Send { filename: "my file.zip".into(), ip: v4(192, 168, 1, 1), port: 5000, size: 123456, token: None, turbo: false, secure: false }
        );
        let plain = parse(&c("send photo.jpg 16777343 6000 10")).unwrap();
        assert!(matches!(plain, Dcc::Send { ref filename, ip, port: 6000, size: 10, .. } if filename == "photo.jpg" && ip == v4(1, 0, 0, 127)));
        assert!(!plain.is_passive());
    }

    #[test]
    fn parses_ipv6_text_tokens_and_passive_offers() {
        let d = parse(&c("SEND f.bin 2001:db8::1 7000 5 tok42")).unwrap();
        assert!(matches!(&d, Dcc::Send { ip: IpAddr::V6(_), token: Some(t), .. } if t == "tok42"));
        let passive = parse(&c("SEND f.bin 0 0 99 12345")).unwrap();
        assert!(passive.is_passive());
        assert!(!parse(&c("SEND f.bin 0 0 99")).unwrap().is_passive(), "port 0 without a token is not passive");
    }

    #[test]
    fn parses_chat() {
        assert_eq!(parse(&c("CHAT chat 2130706433 4000")).unwrap(), Dcc::Chat { ip: v4(127, 0, 0, 1), port: 4000, token: None, secure: false });
        assert!(matches!(parse(&c("CHAT chat 0 0 abc")).unwrap(), Dcc::Chat { token: Some(_), port: 0, .. }));
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        assert_eq!(parse(&Ctcp { command: "VERSION".into(), args: String::new() }), Err(ParseError::NotDcc));
        assert_eq!(parse(&c("")), Err(ParseError::Malformed));
        assert_eq!(parse(&c("SEND onlyname")), Err(ParseError::Malformed));
        assert_eq!(parse(&c("SEND f 99999999999 5000 1")), Err(ParseError::Malformed), "ip overflows u32");
        assert_eq!(parse(&c("SEND f 1 70000 1")), Err(ParseError::Malformed), "port overflows u16");
        assert_eq!(parse(&c("SEND f 1 5000 -4")), Err(ParseError::Malformed));
        assert_eq!(parse(&c("SEND f notanip 5000 1")), Err(ParseError::Malformed));
        assert_eq!(parse(&c("XMIT f 5000 100")), Err(ParseError::Unsupported));
        assert_eq!(parse(&c("RESUME f 5000")), Err(ParseError::Malformed));
        assert_eq!(parse(&c("RESUME f 5000 -1")), Err(ParseError::Malformed));
        assert_eq!(parse(&c("ACCEPT f x 100")), Err(ParseError::Malformed));
        assert_eq!(parse(&c("SEND \"unterminated 1 5000 1")), Err(ParseError::Malformed));
    }

    #[test]
    fn resume_and_accept_roundtrip_with_quotes_and_tokens() {
        let r = resume_request("my file.zip", 5000, 123456, None);
        assert_eq!(r.args, "RESUME \"my file.zip\" 5000 123456");
        assert_eq!(
            parse(&r).unwrap(),
            Dcc::Resume { filename: "my file.zip".into(), port: 5000, position: 123456, token: None }
        );
        let a = accept_reply("f.bin", 0, 99, Some("tok1"));
        assert_eq!(a.args, "ACCEPT f.bin 0 99 tok1");
        assert_eq!(
            parse(&a).unwrap(),
            Dcc::Accept { filename: "f.bin".into(), port: 0, position: 99, token: Some("tok1".into()) }
        );
        // positions beyond u32 survive (files over 4 GB)
        assert!(matches!(parse(&resume_request("big", 5000, 6_000_000_000, None)).unwrap(), Dcc::Resume { position: 6_000_000_000, .. }));
        // resume/accept are never "passive offers" even with port 0 and a token
        assert!(!parse(&a).unwrap().is_passive());
    }

    #[test]
    fn ipv6_addresses_are_classified_and_offers_roundtrip() {
        let public_server = Some(v4(8, 8, 8, 8));
        for private in ["2001:db8::1", "100::1", "fc00::1", "fe80::1", "::1", "::", "ff02::1", "::ffff:10.0.0.1"] {
            assert!(!is_public(private.parse().unwrap()), "{private} must not count as public");
            assert!(target_allowed(private.parse().unwrap(), 5000, public_server).is_err(), "{private}");
        }
        for public in ["2606:2800:220:1::1", "2a00:1450:4001::1", "::ffff:93.184.216.34"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
        // a local IPv6 setup (server on ::1 or a ULA) may be used for local transfers
        let local_server = Some("fd00::1".parse().unwrap());
        assert!(target_allowed("fd00::5".parse().unwrap(), 5000, local_server).is_ok());
        // IPv6 goes into an offer as text and survives the round trip, including the unquoted form
        let ip: IpAddr = "2606:2800:220:1::1".parse().unwrap();
        let o = send_offer("f.bin", ip, 5000, 10, None);
        assert_eq!(o.args, "SEND f.bin 2606:2800:220:1::1 5000 10");
        assert_eq!(parse(&o).unwrap(), Dcc::Send { filename: "f.bin".into(), ip, port: 5000, size: 10, token: None, turbo: false, secure: false });
        let ch = chat_offer(ip, 5001);
        assert_eq!(parse(&ch).unwrap(), Dcc::Chat { ip, port: 5001, token: None, secure: false });
        // an IPv4-mapped address is accepted and is judged by its IPv4 part
        assert!(parse_ip("::ffff:1.2.3.4").is_some());
    }

    #[test]
    fn tsend_is_a_turbo_send_and_keeps_its_verb_when_answering_a_passive_offer() {
        let d = parse(&c("TSEND \"my file.zip\" 3405803785 5000 99")).unwrap();
        assert_eq!(
            d,
            Dcc::Send { filename: "my file.zip".into(), ip: v4(203, 0, 113, 9), port: 5000, size: 99, token: None, turbo: true, secure: false }
        );
        assert!(matches!(parse(&c("tsend f 3405803785 5000 1")).unwrap(), Dcc::Send { turbo: true, .. }), "verbs are case-insensitive");
        assert!(matches!(parse(&c("SEND f 3405803785 5000 1")).unwrap(), Dcc::Send { turbo: false, .. }));
        // encoding and the passive handshake in turbo mode
        let o = send_offer_kind(true, false, "f.bin", v4(203, 0, 113, 9), 5000, 10, None);
        assert_eq!(o.args, "TSEND f.bin 3405803785 5000 10");
        assert_eq!(parse(&o).unwrap(), Dcc::Send { filename: "f.bin".into(), ip: v4(203, 0, 113, 9), port: 5000, size: 10, token: None, turbo: true, secure: false });
        let p = passive_send_offer_kind(true, false, "f.bin", 10, "7");
        assert_eq!(p.args, "TSEND f.bin 0 0 10 7");
        let d = parse(&p).unwrap();
        assert!(d.is_passive() && matches!(d, Dcc::Send { turbo: true, .. }));
        assert_eq!(send_offer("f", v4(1, 2, 3, 4), 5000, 1, None).args, "SEND f 16909060 5000 1", "plain SEND is unchanged");
    }

    #[test]
    fn xmit_clear_with_a_name_and_size_is_a_turbo_receive_and_the_rest_is_unsupported() {
        let d = parse(&c("XMIT clear 3405803785 5000 \"my file.txt\" 4096 text/plain")).unwrap();
        assert_eq!(
            d,
            Dcc::Send { filename: "my file.txt".into(), ip: v4(203, 0, 113, 9), port: 5000, size: 4096, token: None, turbo: true, secure: false }
        );
        assert!(matches!(parse(&c("XMIT CLEAR 2001:db8::1 5000 f.bin 10")).unwrap(), Dcc::Send { ip: IpAddr::V6(_), .. }));
        // another stream protocol (encryption, ...) cannot be handled
        assert_eq!(parse(&c("XMIT rot13 3405803785 5000 f.bin 10")), Err(ParseError::Unsupported));
        // no name or size, or the draft's '-' placeholders: we cannot store an unnamed stream
        assert_eq!(parse(&c("XMIT clear 3405803785 5000")), Err(ParseError::Unsupported));
        assert_eq!(parse(&c("XMIT clear 3405803785 5000 f.bin")), Err(ParseError::Unsupported));
        assert_eq!(parse(&c("XMIT clear 3405803785 5000 - 10")), Err(ParseError::Unsupported));
        assert_eq!(parse(&c("XMIT clear 3405803785 5000 f.bin -")), Err(ParseError::Unsupported));
        // too short, or garbage where numbers belong
        assert_eq!(parse(&c("XMIT clear")), Err(ParseError::Malformed));
        assert_eq!(parse(&c("XMIT clear nonsense 5000 f 10")), Err(ParseError::Malformed));
        assert_eq!(parse(&c("XMIT clear 3405803785 70000 f 10")), Err(ParseError::Malformed));
        assert_eq!(parse(&c("XMIT clear 3405803785 5000 f ten")), Err(ParseError::Malformed));
    }

    #[test]
    fn ssend_and_schat_are_the_secure_verbs_and_stay_secure_through_a_passive_handshake() {
        let d = parse(&c("SSEND \"my file.zip\" 3405803785 5000 99")).unwrap();
        assert_eq!(
            d,
            Dcc::Send { filename: "my file.zip".into(), ip: v4(203, 0, 113, 9), port: 5000, size: 99, token: None, turbo: false, secure: true }
        );
        assert!(matches!(parse(&c("ssend f 3405803785 5000 1")).unwrap(), Dcc::Send { secure: true, .. }), "case-insensitive");
        assert!(matches!(parse(&c("SEND f 3405803785 5000 1")).unwrap(), Dcc::Send { secure: false, .. }));
        assert!(matches!(parse(&c("TSEND f 3405803785 5000 1")).unwrap(), Dcc::Send { secure: false, turbo: true, .. }));
        let o = send_offer_kind(false, true, "f.bin", v4(203, 0, 113, 9), 5000, 10, None);
        assert_eq!(o.args, "SSEND f.bin 3405803785 5000 10");
        assert_eq!(send_offer_kind(true, true, "f", v4(1, 2, 3, 4), 5000, 1, None).args, "SSEND f 16909060 5000 1", "secure wins over turbo: there is no combined verb");
        let p = passive_send_offer_kind(false, true, "f.bin", 10, "7");
        assert_eq!(p.args, "SSEND f.bin 0 0 10 7");
        assert!(parse(&p).unwrap().is_passive());
        // chat
        let ch = parse(&c("SCHAT chat 3405803785 4000")).unwrap();
        assert_eq!(ch, Dcc::Chat { ip: v4(203, 0, 113, 9), port: 4000, token: None, secure: true });
        assert!(matches!(parse(&c("CHAT chat 3405803785 4000")).unwrap(), Dcc::Chat { secure: false, .. }));
        assert_eq!(chat_offer_kind(true, v4(10, 0, 0, 5), 4040).args, "SCHAT chat 167772165 4040");
        assert_eq!(chat_offer(v4(10, 0, 0, 5), 4040).args, "CHAT chat 167772165 4040", "plain chat is unchanged");
        let pc = passive_chat_offer_kind(true, "88");
        assert_eq!(pc.args, "SCHAT chat 0 0 88");
        assert!(parse(&pc).unwrap().is_passive());
        assert_eq!(chat_offer_token_kind(true, v4(203, 0, 113, 9), 4000, "88").args, "SCHAT chat 3405803785 4000 88");
    }

    #[test]
    fn passive_offers_and_replies_roundtrip() {
        let p = passive_send_offer("my file.zip", 1234, "77");
        assert_eq!(p.args, "SEND \"my file.zip\" 0 0 1234 77");
        let d = parse(&p).unwrap();
        assert!(d.is_passive());
        assert!(matches!(d, Dcc::Send { port: 0, size: 1234, token: Some(ref t), .. } if t == "77"));
        let pc = passive_chat_offer("88");
        assert_eq!(pc.args, "CHAT chat 0 0 88");
        assert!(parse(&pc).unwrap().is_passive());
        // the answer carries a real address and port and the same token, and is not passive
        let r = chat_offer_token(v4(203, 0, 113, 9), 4000, "88");
        assert_eq!(r.args, "CHAT chat 3405803785 4000 88");
        let d = parse(&r).unwrap();
        assert!(!d.is_passive());
        assert_eq!(d, Dcc::Chat { ip: v4(203, 0, 113, 9), port: 4000, token: Some("88".into()), secure: false });
    }

    #[test]
    fn encode_roundtrips() {
        let o = send_offer("my file.txt", v4(203, 0, 113, 9), 5555, 42, None);
        assert_eq!(o.encode(), "\x01DCC SEND \"my file.txt\" 3405803785 5555 42\x01");
        // answering a passive offer echoes the peer's token
        let reply = send_offer("f.bin", v4(203, 0, 113, 9), 5556, 99, Some("12345"));
        assert_eq!(reply.args, "SEND f.bin 3405803785 5556 99 12345");
        assert!(matches!(parse(&reply).unwrap(), Dcc::Send { token: Some(t), .. } if t == "12345"));
        assert_eq!(
            parse(&o).unwrap(),
            Dcc::Send { filename: "my file.txt".into(), ip: v4(203, 0, 113, 9), port: 5555, size: 42, token: None, turbo: false, secure: false }
        );
        let ch = chat_offer(v4(10, 0, 0, 5), 4040);
        assert_eq!(parse(&ch).unwrap(), Dcc::Chat { ip: v4(10, 0, 0, 5), port: 4040, token: None, secure: false });
        let v6 = chat_offer("2001:db8::2".parse().unwrap(), 4041);
        assert!(matches!(parse(&v6).unwrap(), Dcc::Chat { ip: IpAddr::V6(_), port: 4041, .. }));
    }

    #[test]
    fn filenames_are_made_safe() {
        for evil in ["..\\..\\Windows\\system32\\evil.exe", "../../etc/passwd", "C:\\x\\y.txt", "a/b/c.bin", "dir\\"] {
            let s = safe_filename(evil);
            assert!(!s.contains(['/', '\\', ':']), "{evil} -> {s}");
            assert!(!s.is_empty() && !s.starts_with('.'));
        }
        assert_eq!(safe_filename("..\\..\\boot.ini"), "boot.ini");
        assert_eq!(safe_filename("report?.pdf"), "report_.pdf");
        assert_eq!(safe_filename("con"), "_con");
        assert_eq!(safe_filename("NUL.txt"), "_NUL.txt");
        assert_eq!(safe_filename("name."), "name");
        assert_eq!(safe_filename(""), "download");
        assert_eq!(safe_filename("..."), "download");
        assert_eq!(safe_filename(".hidden"), "_hidden");
        assert_eq!(safe_filename("Keep Case.TXT"), "Keep Case.TXT");
        let long = format!("{}.tar.gz", "x".repeat(300));
        let s = safe_filename(&long);
        assert_eq!(s.chars().count(), 120);
        assert!(s.ends_with(".gz"));
        assert_eq!(safe_filename("tab\tname\u{7}.txt"), "tab_name_.txt");
    }

    #[test]
    fn risky_extensions() {
        for r in ["setup.EXE", "a.scr", "x.ps1", "doc.docm", "a.b.bat"] {
            assert!(is_risky_extension(r), "{r}");
        }
        for ok in ["photo.jpg", "notes.txt", "archive.zip", "noext", "exe"] {
            assert!(!is_risky_extension(ok), "{ok}");
        }
    }

    #[test]
    fn bounce_protection_blocks_probing_of_private_addresses() {
        let public_server = Some(v4(8, 8, 8, 8));
        // a public IRC server must never be able to aim us at local or private addresses
        for target in [v4(127, 0, 0, 1), v4(192, 168, 1, 1), v4(10, 0, 0, 7), v4(172, 16, 0, 1), v4(169, 254, 169, 254), v4(100, 64, 0, 1), IpAddr::V6(Ipv6Addr::LOCALHOST), "fd00::1".parse().unwrap(), "fe80::1".parse().unwrap(), "::ffff:127.0.0.1".parse().unwrap()] {
            assert!(target_allowed(target, 5000, public_server).is_err(), "{target}");
        }
        assert!(target_allowed(v4(93, 184, 216, 34), 5000, public_server).is_ok());
        assert!(target_allowed("2606:2800:220:1::1".parse().unwrap(), 5000, public_server).is_ok());
        // unknown server address is treated as public (safe default)
        assert!(target_allowed(v4(127, 0, 0, 1), 5000, None).is_err());
        // low ports, never
        assert!(target_allowed(v4(93, 184, 216, 34), 22, public_server).is_err());
        assert!(target_allowed(v4(93, 184, 216, 34), 1023, public_server).is_err());
        assert!(target_allowed(v4(93, 184, 216, 34), 1024, public_server).is_ok());
        // unusable addresses, never
        assert!(target_allowed(v4(0, 0, 0, 0), 5000, Some(v4(127, 0, 0, 1))).is_err());
        assert!(target_allowed(v4(224, 0, 0, 1), 5000, Some(v4(127, 0, 0, 1))).is_err());
        assert!(target_allowed(v4(255, 255, 255, 255), 5000, Some(v4(127, 0, 0, 1))).is_err());
        // a genuine LAN/local setup (server itself is private/loopback) may use local peers
        assert!(target_allowed(v4(192, 168, 1, 20), 5000, Some(v4(192, 168, 1, 2))).is_ok());
        assert!(target_allowed(v4(127, 0, 0, 1), 5000, Some(v4(127, 0, 0, 1))).is_ok());
    }
}
