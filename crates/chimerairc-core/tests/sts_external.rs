//! IRCv3 Strict Transport Security and SASL EXTERNAL through the session.

use rustls::pki_types::PrivateKeyDer;
use chimerairc_core::clientcert::ClientCert;
use chimerairc_core::{Config, Event, Session};
use chimerairc_proto::Message;

fn feed(s: &mut Session, line: &str) {
    s.handle(&Message::parse(line).unwrap());
}

fn wire(s: &mut Session) -> Vec<String> {
    s.take_outbox().iter().map(|m| m.encode_wire().unwrap()).collect()
}

fn cert() -> ClientCert {
    let ck = rcgen::generate_simple_self_signed(vec!["me.example".to_owned()]).unwrap();
    let key = PrivateKeyDer::try_from(ck.signing_key.serialize_der()).unwrap();
    ClientCert::from_der(vec![ck.cert.der().clone()], key).unwrap()
}

fn plaintext(mut cfg: Config) -> Config {
    cfg.tls = false;
    cfg
}

fn started(cfg: Config) -> (Session, Vec<String>) {
    let mut s = Session::new(cfg);
    s.start();
    let first = wire(&mut s);
    (s, first)
}

// ------------------------------------------------------------------------------------- STS

#[test]
fn over_tls_a_policy_is_reported_for_the_ui_to_remember() {
    let mut cfg = Config::new("irc.example", "me");
    cfg.port = 6697;
    let (mut s, _) = started(cfg);
    feed(&mut s, ":srv CAP * LS :sts=duration=2592000,port=6697,preload multi-prefix");
    let out = wire(&mut s);
    assert!(out.contains(&"CAP REQ multi-prefix".to_owned()), "{out:?}");
    assert!(!out.iter().any(|l| l.contains("sts")), "sts is information, never requested: {out:?}");
    let ev = s.take_events();
    assert!(
        ev.iter().any(|e| matches!(e, Event::StsPolicy { duration: 2_592_000, port: 6697, preload: true })),
        "{ev:?}"
    );
    assert!(!s.is_upgrading());
}

#[test]
fn a_duration_of_zero_withdraws_a_policy_and_a_port_alone_means_nothing_over_tls() {
    let (mut s, _) = started(Config::new("h", "me"));
    feed(&mut s, ":srv CAP * LS :sts=duration=0");
    assert!(s.take_events().iter().any(|e| matches!(e, Event::StsPolicy { duration: 0, .. })));
    let (mut s, _) = started(Config::new("h", "me"));
    feed(&mut s, ":srv CAP * LS :sts=port=6697");
    assert!(!s.take_events().iter().any(|e| matches!(e, Event::StsPolicy { .. } | Event::StsUpgrade { .. })));
    assert!(!s.is_upgrading(), "already on TLS: nothing to upgrade");
}

#[test]
fn over_plaintext_a_port_means_reconnect_with_tls_and_nothing_private_was_sent() {
    let mut cfg = plaintext(Config::new("irc.example", "me"));
    cfg.password = Some("hunter2".into());
    cfg.sasl = Some(("acct".into(), "pw".into()));
    let (mut s, first) = started(cfg);
    assert_eq!(first, ["CAP LS 302"], "only CAP LS goes out on plaintext before the server answers: {first:?}");
    feed(&mut s, ":srv CAP * LS :sts=port=6697 sasl=PLAIN multi-prefix");
    assert!(s.is_upgrading());
    assert!(matches!(s.take_events().as_slice(), [Event::StsUpgrade { port: 6697 }]));
    let out = wire(&mut s);
    assert!(out.is_empty(), "no PASS, no NICK, no credentials, no CAP REQ: {out:?}");
    // the session is finished: whatever the server says next changes nothing
    feed(&mut s, ":srv CAP me ACK sasl");
    feed(&mut s, ":srv 001 me :Welcome");
    feed(&mut s, "PING :x");
    assert!(wire(&mut s).is_empty() && s.take_events().is_empty());
}

#[test]
fn a_duration_without_a_port_over_plaintext_is_ignored_and_a_multiline_list_still_works() {
    let (mut s, _) = started(plaintext(Config::new("h", "me")));
    feed(&mut s, ":srv CAP * LS :sts=duration=3600 multi-prefix");
    assert!(!s.is_upgrading(), "a policy over plaintext is not valid and must not be obeyed");
    assert!(!s.take_events().iter().any(|e| matches!(e, Event::StsPolicy { .. } | Event::StsUpgrade { .. })));
    // an `sts` on the first line of several is acted on at once
    let (mut s, _) = started(plaintext(Config::new("h", "me")));
    feed(&mut s, ":srv CAP * LS * :sts=port=7000");
    assert!(s.is_upgrading());
}

#[test]
fn plaintext_with_a_login_holds_back_registration_until_cap_ls_is_answered() {
    let mut cfg = plaintext(Config::new("h", "me"));
    cfg.password = Some("pw".into());
    let (mut s, first) = started(cfg.clone());
    assert_eq!(first, ["CAP LS 302"]);
    feed(&mut s, ":srv CAP * LS :multi-prefix");
    assert_eq!(wire(&mut s), ["PASS pw", "NICK me", "USER me 0 * me", "CAP REQ multi-prefix"]);
    // a server that does not know CAP at all
    let (mut s, _) = started(cfg);
    feed(&mut s, ":srv 421 me CAP :Unknown command");
    assert_eq!(wire(&mut s), ["PASS pw", "NICK me", "USER me 0 * me"]);
    // no secrets, or TLS: registration is sent at once as before
    let (_, first) = started(plaintext(Config::new("h", "me")));
    assert_eq!(first, ["CAP LS 302", "NICK me", "USER me 0 * me"]);
    let mut tls = Config::new("h", "me");
    tls.password = Some("pw".into());
    let (_, first) = started(tls);
    assert_eq!(first, ["CAP LS 302", "PASS pw", "NICK me", "USER me 0 * me"]);
}

#[test]
fn a_policy_over_tls_is_reported_once_even_if_caps_are_announced_again() {
    let mut s = {
        let (mut s, _) = started(Config::new("h", "me"));
        feed(&mut s, ":srv CAP * LS :sts=duration=100");
        s.take_events();
        s
    };
    feed(&mut s, ":srv CAP me NEW :sts=duration=200 away-notify");
    assert!(!s.take_events().iter().any(|e| matches!(e, Event::StsPolicy { .. })));
}

// ------------------------------------------------------------------------------------- EXTERNAL

fn external_cfg(with_creds: bool) -> Config {
    let mut cfg = Config::new("h", "me");
    cfg.client_cert = Some(cert());
    if with_creds {
        cfg.sasl = Some(("user".into(), "pw".into()));
    }
    cfg
}

fn sasl_lines(cfg: Config, offered: &str) -> (Session, Vec<String>) {
    let (mut s, _) = started(cfg);
    feed(&mut s, &format!(":srv CAP * LS :sasl{offered}"));
    let mut lines = wire(&mut s);
    feed(&mut s, ":srv CAP me ACK sasl");
    lines.extend(wire(&mut s));
    (s, lines)
}

#[test]
fn external_logs_in_with_the_certificate_alone() {
    let (mut s, lines) = sasl_lines(external_cfg(false), "=EXTERNAL,PLAIN");
    assert_eq!(lines, ["CAP REQ sasl", "AUTHENTICATE EXTERNAL"]);
    feed(&mut s, "AUTHENTICATE +");
    assert_eq!(wire(&mut s), ["AUTHENTICATE +"], "an empty authorisation identity");
    feed(&mut s, ":srv 900 me me!u@h acct :You are now logged in as acct");
    feed(&mut s, ":srv 903 me :SASL authentication successful");
    assert_eq!(wire(&mut s), ["CAP END"]);
    let ev = s.take_events();
    assert!(ev.iter().any(|e| matches!(e, Event::SaslResult { ok: true, text } if text.contains("EXTERNAL"))), "{ev:?}");
}

#[test]
fn the_certificate_wins_over_a_password_when_the_server_accepts_it() {
    let (_, lines) = sasl_lines(external_cfg(true), "=PLAIN,SCRAM-SHA-256,EXTERNAL");
    assert_eq!(lines, ["CAP REQ sasl", "AUTHENTICATE EXTERNAL"]);
    // without EXTERNAL on offer the password is used as before
    let (_, lines) = sasl_lines(external_cfg(true), "=PLAIN,SCRAM-SHA-256");
    assert_eq!(lines, ["CAP REQ sasl", "AUTHENTICATE SCRAM-SHA-256"]);
    let (_, lines) = sasl_lines(external_cfg(true), "=PLAIN");
    assert_eq!(lines, ["CAP REQ sasl", "AUTHENTICATE PLAIN"]);
}

#[test]
fn a_certificate_alone_is_not_enough_when_the_server_does_not_offer_external() {
    let (mut s, _) = started(external_cfg(false));
    feed(&mut s, ":srv CAP * LS :sasl=PLAIN,SCRAM-SHA-256");
    assert_eq!(wire(&mut s), ["CAP END"], "no SASL request, and no credentials exist to send anyway");
    assert!(s.take_events().iter().any(|e| matches!(e, Event::SaslResult { ok: false, text } if text.contains("EXTERNAL"))));
}

#[test]
fn without_tls_the_certificate_is_never_used() {
    let mut cfg = plaintext(external_cfg(false));
    cfg.sasl = None;
    let (mut s, _) = started(cfg);
    feed(&mut s, ":srv CAP * LS :sasl=EXTERNAL");
    assert_eq!(wire(&mut s), ["CAP END"], "a certificate only exists inside a TLS handshake");
    // with a password too, plaintext falls back to what the password allows
    let mut cfg = plaintext(external_cfg(true));
    cfg.client_cert = Some(cert());
    let (_, lines) = sasl_lines(cfg, "=EXTERNAL,PLAIN");
    // (on plaintext, registration follows the server's CAP LS answer)
    assert_eq!(lines, ["NICK me", "USER me 0 * me", "CAP REQ sasl", "AUTHENTICATE PLAIN"]);
}

#[test]
fn a_server_that_lists_no_mechanisms_gets_an_external_try_only_when_there_is_no_password() {
    let (_, lines) = sasl_lines(external_cfg(false), "");
    assert_eq!(lines, ["CAP REQ sasl", "AUTHENTICATE EXTERNAL"]);
    let (_, lines) = sasl_lines(external_cfg(true), "");
    assert_eq!(lines, ["CAP REQ sasl", "AUTHENTICATE PLAIN"], "the old behaviour with a password");
}

#[test]
fn a_failed_external_never_falls_back_to_the_password() {
    let (mut s, _) = sasl_lines(external_cfg(true), "=EXTERNAL,PLAIN");
    feed(&mut s, "AUTHENTICATE +");
    wire(&mut s);
    feed(&mut s, ":srv 904 me :SASL authentication failed");
    let out = wire(&mut s);
    assert_eq!(out, ["CAP END"], "{out:?}");
    assert!(s.take_events().iter().any(|e| matches!(e, Event::SaslResult { ok: false, .. })));
}
