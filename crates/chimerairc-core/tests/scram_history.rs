//! SASL SCRAM-SHA-256 through the session, and IRCv3 `batch` / `chathistory`.

use base64::Engine;
use chimerairc_core::scram::Scram;
use chimerairc_core::{ChatKind, Config, Event, IgnoreRule, Session};
use chimerairc_proto::Message;

fn feed(s: &mut Session, line: &str) {
    s.handle(&Message::parse(line).unwrap());
}

fn wire(s: &mut Session) -> Vec<String> {
    s.take_outbox().iter().map(|m| m.encode_wire().unwrap()).collect()
}

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
}

fn fixed_nonce() -> String {
    "rOprNGfwEbeRWgbNEkqO".to_owned()
}

// The worked example of RFC 7677, section 3.
const SERVER_FIRST: &str = "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
const CLIENT_FINAL: &str = "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=";
const SERVER_FINAL: &str = "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=";

fn scram_cfg(password: &str) -> Config {
    let mut cfg = Config::new("h", "me");
    cfg.sasl = Some(("user".into(), password.into()));
    cfg.scram_nonce = fixed_nonce;
    cfg
}

/// A session that has asked for SASL on a server offering `mechs` and been told yes.
fn sasl_session(cfg: Config, mechs: &str) -> (Session, Vec<String>) {
    let mut s = Session::new(cfg);
    s.start();
    s.take_outbox();
    feed(&mut s, &format!(":srv CAP * LS :sasl{mechs}"));
    let req = wire(&mut s);
    feed(&mut s, ":srv CAP me ACK sasl");
    let after_ack = wire(&mut s);
    (s, [req, after_ack].concat())
}

#[test]
fn a_full_scram_login_matches_the_rfc_7677_exchange_and_ends_cap_negotiation() {
    let (mut s, first) = sasl_session(scram_cfg("pencil"), "=PLAIN,SCRAM-SHA-256");
    assert_eq!(first, ["CAP REQ sasl", "AUTHENTICATE SCRAM-SHA-256"], "SCRAM wins over PLAIN");
    feed(&mut s, "AUTHENTICATE +");
    assert_eq!(wire(&mut s), [format!("AUTHENTICATE {}", b64("n,,n=user,r=rOprNGfwEbeRWgbNEkqO"))]);
    feed(&mut s, &format!("AUTHENTICATE {}", b64(SERVER_FIRST)));
    assert_eq!(wire(&mut s), [format!("AUTHENTICATE {}", b64(CLIENT_FINAL))]);
    feed(&mut s, &format!("AUTHENTICATE {}", b64(SERVER_FINAL)));
    assert_eq!(wire(&mut s), ["AUTHENTICATE +"], "the empty final response");
    feed(&mut s, ":srv 900 me me!u@h user :You are now logged in as user");
    feed(&mut s, ":srv 903 me :SASL authentication successful");
    assert_eq!(wire(&mut s), ["CAP END"]);
    let ev = s.take_events();
    assert!(ev.iter().any(|e| matches!(e, Event::SaslResult { ok: true, text } if text.contains("SCRAM-SHA-256"))), "{ev:?}");
    // the password itself never crossed the wire
}

#[test]
fn the_password_is_never_sent_during_scram() {
    let (mut s, mut lines) = sasl_session(scram_cfg("pencil"), "=SCRAM-SHA-256");
    feed(&mut s, "AUTHENTICATE +");
    lines.extend(wire(&mut s));
    feed(&mut s, &format!("AUTHENTICATE {}", b64(SERVER_FIRST)));
    lines.extend(wire(&mut s));
    let all = lines.join("\n");
    assert!(!all.contains("pencil") && !all.contains(&b64("pencil")), "the password must not appear: {all}");
}

#[test]
fn a_server_that_cannot_prove_it_knows_the_password_is_refused() {
    let (mut s, _) = sasl_session(scram_cfg("pencil"), "=SCRAM-SHA-256");
    feed(&mut s, "AUTHENTICATE +");
    wire(&mut s);
    feed(&mut s, &format!("AUTHENTICATE {}", b64(SERVER_FIRST)));
    wire(&mut s);
    // a signature made with another password
    feed(&mut s, &format!("AUTHENTICATE {}", b64("v=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")));
    assert_eq!(wire(&mut s), ["AUTHENTICATE *", "CAP END"], "abort, then carry on without logging in");
    let ev = s.take_events();
    assert!(ev.iter().any(|e| matches!(e, Event::SaslResult { ok: false, text } if text.contains("did not prove"))), "{ev:?}");
}

#[test]
fn a_903_without_the_servers_proof_is_reported_as_a_failure() {
    let (mut s, _) = sasl_session(scram_cfg("pencil"), "=SCRAM-SHA-256");
    feed(&mut s, "AUTHENTICATE +");
    wire(&mut s);
    feed(&mut s, ":srv 903 me :SASL authentication successful"); // before any proof
    let ev = s.take_events();
    assert!(
        ev.iter().any(|e| matches!(e, Event::SaslResult { ok: false, text } if text.contains("without proving"))),
        "{ev:?}"
    );
}

#[test]
fn bad_server_challenges_abort_the_login_cleanly() {
    for (label, server_first) in [
        ("foreign nonce", "r=somebodyElsesNonce,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096"),
        ("too few iterations", "r=rOprNGfwEbeRWgbNEkqOabc,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=10"),
        ("server error", "e=unknown-user"),
        ("junk", "this is not a scram message"),
    ] {
        let (mut s, _) = sasl_session(scram_cfg("pencil"), "=SCRAM-SHA-256");
        feed(&mut s, "AUTHENTICATE +");
        wire(&mut s);
        feed(&mut s, &format!("AUTHENTICATE {}", b64(server_first)));
        assert_eq!(wire(&mut s), ["AUTHENTICATE *", "CAP END"], "{label}");
        assert!(s.take_events().iter().any(|e| matches!(e, Event::SaslResult { ok: false, .. })), "{label}");
    }
    // data that is not even base64
    let (mut s, _) = sasl_session(scram_cfg("pencil"), "=SCRAM-SHA-256");
    feed(&mut s, "AUTHENTICATE +");
    wire(&mut s);
    feed(&mut s, "AUTHENTICATE !!!not-base64!!!");
    assert_eq!(wire(&mut s), ["AUTHENTICATE *", "CAP END"]);
}

#[test]
fn a_failed_scram_never_falls_back_to_plain() {
    let (mut s, _) = sasl_session(scram_cfg("pencil"), "=PLAIN,SCRAM-SHA-256");
    feed(&mut s, "AUTHENTICATE +");
    wire(&mut s);
    feed(&mut s, ":srv 904 me :SASL authentication failed");
    let out = wire(&mut s);
    assert_eq!(out, ["CAP END"], "no second attempt with PLAIN: {out:?}");
    assert!(!out.iter().any(|l| l.contains("PLAIN")));
}

#[test]
fn a_long_server_message_in_several_lines_is_reassembled() {
    let (mut s, _) = sasl_session(scram_cfg("pencil"), "=SCRAM-SHA-256");
    feed(&mut s, "AUTHENTICATE +");
    wire(&mut s);
    // a server nonce long enough that the base64 of the message needs three AUTHENTICATE lines
    let server_nonce = "x".repeat(700);
    let sf = format!("r=rOprNGfwEbeRWgbNEkqO{server_nonce},s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096");
    let enc = b64(&sf);
    assert!(enc.len() > 800);
    for chunk in enc.as_bytes().chunks(400) {
        feed(&mut s, &format!("AUTHENTICATE {}", std::str::from_utf8(chunk).unwrap()));
    }
    // the answer is what the SCRAM module computes for that message, sent in 400-character lines
    let expect = Scram::new("user", "pencil", "rOprNGfwEbeRWgbNEkqO").client_final(&sf).unwrap();
    let want = b64(&expect);
    let got: String = wire(&mut s).iter().filter_map(|l| l.strip_prefix("AUTHENTICATE ")).filter(|p| *p != "+").collect();
    assert_eq!(got, want, "the reassembled challenge must give exactly the proof SCRAM computes for it");
    assert!(s.take_events().iter().all(|e| !matches!(e, Event::SaslResult { ok: false, .. })), "must not abort");
}

#[test]
fn mechanism_choice_follows_what_the_server_offers_and_what_the_credentials_allow() {
    let first_lines = |cfg: Config, offered: &str| sasl_session(cfg, offered).1;
    assert_eq!(first_lines(scram_cfg("pw"), "=PLAIN"), ["CAP REQ sasl", "AUTHENTICATE PLAIN"]);
    assert_eq!(first_lines(scram_cfg("pw"), ""), ["CAP REQ sasl", "AUTHENTICATE PLAIN"], "no list: the old behaviour");
    assert_eq!(first_lines(scram_cfg("pw"), "=scram-sha-256"), ["CAP REQ sasl", "AUTHENTICATE SCRAM-SHA-256"], "case-insensitive");
    // credentials that SCRAM here cannot handle (non-ASCII) use PLAIN when it is offered
    assert_eq!(
        first_lines(scram_cfg("pässword"), "=PLAIN,SCRAM-SHA-256"),
        ["CAP REQ sasl", "AUTHENTICATE PLAIN"]
    );
    // ... and nothing is sent when PLAIN is not an option either
    let mut s = Session::new(scram_cfg("pässword"));
    s.start();
    s.take_outbox();
    feed(&mut s, ":srv CAP * LS :sasl=SCRAM-SHA-256");
    assert_eq!(wire(&mut s), ["CAP END"]);
    // a server with neither mechanism: no request, never any credentials
    let mut s = Session::new(scram_cfg("pw"));
    s.start();
    s.take_outbox();
    feed(&mut s, ":srv CAP * LS :sasl=EXTERNAL");
    assert_eq!(wire(&mut s), ["CAP END"]);
    assert!(s.take_events().iter().any(|e| matches!(e, Event::SaslResult { ok: false, .. })));
}

// ------------------------------------------------------------------------------- chathistory

fn history_session(extra: &str, cfg: Config) -> Session {
    let mut s = Session::new(cfg);
    s.start();
    s.take_outbox();
    feed(&mut s, &format!(":srv CAP * LS :{extra}"));
    for req in wire(&mut s) {
        if let Some(cap) = req.strip_prefix("CAP REQ ") {
            feed(&mut s, &format!(":srv CAP me ACK {cap}"));
        }
    }
    s.take_outbox();
    feed(&mut s, ":srv 001 me :Welcome");
    feed(&mut s, ":srv 005 me PREFIX=(ov)@+ CHANTYPES=# :are supported");
    s.take_events();
    s
}

#[test]
fn history_is_requested_only_with_batch_and_in_its_final_name_first() {
    let reqs = |extra: &str| {
        let mut s = Session::new(Config::new("h", "me"));
        s.start();
        s.take_outbox();
        feed(&mut s, &format!(":srv CAP * LS :{extra}"));
        wire(&mut s)
    };
    let r = reqs("batch server-time chathistory draft/chathistory");
    assert!(r.contains(&"CAP REQ chathistory".to_owned()) && !r.contains(&"CAP REQ draft/chathistory".to_owned()), "{r:?}");
    assert!(r.contains(&"CAP REQ batch".to_owned()));
    let r = reqs("batch draft/chathistory");
    assert!(r.contains(&"CAP REQ draft/chathistory".to_owned()), "{r:?}");
    let r = reqs("chathistory server-time");
    assert!(!r.iter().any(|l| l.contains("chathistory")), "no batch, no history: {r:?}");
    let mut cfg = Config::new("h", "me");
    cfg.history_on_join = 0;
    let mut s = Session::new(cfg);
    s.start();
    s.take_outbox();
    feed(&mut s, ":srv CAP * LS :batch chathistory");
    let r = wire(&mut s);
    assert!(!r.iter().any(|l| l.contains("chathistory")), "history switched off: {r:?}");
}

#[test]
fn joining_a_channel_asks_for_the_latest_lines_within_the_servers_limit() {
    let mut s = history_session("batch server-time message-tags chathistory", Config::new("h", "me"));
    feed(&mut s, ":me!u@h JOIN #c");
    assert_eq!(wire(&mut s), ["CHATHISTORY LATEST #c * 50"]);
    // someone else joining does not trigger a request
    feed(&mut s, ":bob!u@h JOIN #c");
    assert!(wire(&mut s).is_empty());
    // the server's own limit lowers the request
    let mut s = history_session("batch chathistory", Config::new("h", "me"));
    feed(&mut s, ":srv 005 me CHATHISTORY=20 :are supported");
    feed(&mut s, ":me!u@h JOIN #d");
    assert_eq!(wire(&mut s), ["CHATHISTORY LATEST #d * 20"]);
    // and a server without the capability is never asked
    let mut s = history_session("batch", Config::new("h", "me"));
    feed(&mut s, ":me!u@h JOIN #e");
    assert!(wire(&mut s).is_empty());
}

#[test]
fn a_history_batch_arrives_as_one_event_and_not_as_live_chat() {
    let mut s = history_session("batch server-time message-tags chathistory", Config::new("h", "me"));
    feed(&mut s, ":srv BATCH +abc chathistory #c");
    feed(&mut s, "@batch=abc;time=2024-05-05T10:00:00.000Z;msgid=m1 :bob!b@h PRIVMSG #c :hello");
    feed(&mut s, "@batch=abc;time=2024-05-05T10:00:05.000Z;msgid=m2 :bob!b@h PRIVMSG #c :\x01ACTION waves\x01");
    feed(&mut s, "@batch=abc;time=2024-05-05T10:00:09.000Z;msgid=m3 :srv NOTICE #c :a notice");
    feed(&mut s, "@batch=abc;time=2024-05-05T10:00:10.000Z :bob!b@h PRIVMSG #c :\x01VERSION\x01"); // CTCP: not history
    feed(&mut s, ":srv BATCH -abc");
    assert!(wire(&mut s).is_empty(), "no automatic CTCP replies to history");
    let ev = s.take_events();
    assert_eq!(ev.len(), 1, "one event for the whole batch: {ev:?}");
    let Event::History { target, lines } = &ev[0] else { panic!("{ev:?}") };
    assert_eq!(target, "#c");
    assert_eq!(lines.len(), 3);
    assert_eq!((lines[0].text.as_str(), lines[0].kind, lines[0].msgid.as_deref()), ("hello", ChatKind::Message, Some("m1")));
    assert_eq!((lines[1].text.as_str(), lines[1].kind), ("waves", ChatKind::Action));
    assert_eq!(lines[2].kind, ChatKind::Notice);
    assert_eq!(lines[0].time.as_deref(), Some("2024-05-05T10:00:00.000Z"));
    assert_eq!(lines[0].source.name, "bob");
}

#[test]
fn live_messages_carry_their_msgid_and_other_batches_are_left_alone() {
    let mut s = history_session("batch server-time message-tags chathistory", Config::new("h", "me"));
    feed(&mut s, "@msgid=live1 :bob!b@h PRIVMSG #c :hi");
    assert!(matches!(&s.take_events()[0], Event::Chat { msgid: Some(id), .. } if id == "live1"));
    // a netsplit batch is not history: its lines are processed as usual
    feed(&mut s, ":srv BATCH +ns netsplit a.example b.example");
    feed(&mut s, "@batch=ns :bob!b@h PRIVMSG #c :still delivered");
    feed(&mut s, ":srv BATCH -ns");
    let ev = s.take_events();
    assert!(ev.iter().any(|e| matches!(e, Event::Chat { text, .. } if text == "still delivered")));
    assert!(!ev.iter().any(|e| matches!(e, Event::History { .. })));
    // a batch tag naming a batch we never saw does not hide the message
    feed(&mut s, "@batch=ghost :bob!b@h PRIVMSG #c :orphan");
    assert!(s.take_events().iter().any(|e| matches!(e, Event::Chat { text, .. } if text == "orphan")));
    // closing a batch that was never opened is harmless
    feed(&mut s, ":srv BATCH -nothing");
    assert!(s.take_events().is_empty());
}

#[test]
fn an_empty_batch_still_reports_so_the_ui_can_say_there_is_no_history() {
    let mut s = history_session("batch chathistory", Config::new("h", "me"));
    feed(&mut s, ":srv BATCH +e chathistory #quiet");
    feed(&mut s, ":srv BATCH -e");
    assert!(matches!(&s.take_events()[0], Event::History { target, lines } if target == "#quiet" && lines.is_empty()));
}

#[test]
fn ignored_people_do_not_appear_in_history() {
    let mut cfg = Config::new("h", "me");
    cfg.ignores = vec![IgnoreRule::new("*!*@evil.host")];
    let mut s = history_session("batch chathistory", cfg);
    feed(&mut s, ":srv BATCH +i chathistory #c");
    feed(&mut s, "@batch=i :bad!u@evil.host PRIVMSG #c :spam");
    feed(&mut s, "@batch=i :good!u@fine.host PRIVMSG #c :fine");
    feed(&mut s, ":srv BATCH -i");
    let ev = s.take_events();
    let Event::History { lines, .. } = &ev[0] else { panic!("{ev:?}") };
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text, "fine");
}

#[test]
fn a_server_cannot_make_us_buffer_without_limit() {
    let mut s = history_session("batch chathistory", Config::new("h", "me"));
    feed(&mut s, ":srv BATCH +big chathistory #c");
    for i in 0..2500 {
        feed(&mut s, &format!("@batch=big :bob!b@h PRIVMSG #c :line {i}"));
    }
    feed(&mut s, ":srv BATCH -big");
    let ev = s.take_events();
    let Event::History { lines, .. } = &ev[0] else { panic!() };
    assert_eq!(lines.len(), 2000, "capped");
    // many batches at once: only the first few are tracked
    for i in 0..20 {
        feed(&mut s, &format!(":srv BATCH +b{i} chathistory #c"));
    }
    for i in 0..20 {
        feed(&mut s, &format!(":srv BATCH -b{i}"));
    }
    let n = s.take_events().iter().filter(|e| matches!(e, Event::History { .. })).count();
    assert_eq!(n, 8);
}
