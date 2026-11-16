use chimerairc_core::{ChatKind, Config, Event, Session};
use chimerairc_proto::Message;

fn feed(s: &mut Session, line: &str) {
    s.handle(&Message::parse(line).unwrap());
}

fn wire(s: &mut Session) -> Vec<String> {
    s.take_outbox().iter().map(|m| m.encode_wire().unwrap()).collect()
}

fn registered(cfg: Config) -> Session {
    let mut s = Session::new(cfg);
    s.start();
    s.take_outbox();
    feed(&mut s, ":srv 001 me :Welcome");
    feed(&mut s, ":srv 005 me PREFIX=(ov)@+ CHANMODES=b,k,l,imnpst CASEMAPPING=rfc1459 :are supported");
    s.take_events();
    s
}

#[test]
fn start_sends_cap_ls_then_registration() {
    let mut cfg = Config::new("irc.example", "me");
    cfg.password = Some("pw".into());
    let mut s = Session::new(cfg);
    s.start();
    assert_eq!(wire(&mut s), ["CAP LS 302", "PASS pw", "NICK me", "USER me 0 * me"]);
}

#[test]
fn multiline_ls_waits_then_requests_and_ends() {
    let mut s = Session::new(Config::new("h", "me"));
    s.start();
    s.take_outbox();
    feed(&mut s, ":srv CAP * LS * :multi-prefix");
    assert!(wire(&mut s).is_empty(), "must wait for the final LS line");
    feed(&mut s, ":srv CAP * LS :server-time something-else");
    assert_eq!(wire(&mut s), ["CAP REQ multi-prefix", "CAP REQ server-time"]);
    feed(&mut s, ":srv CAP me ACK multi-prefix");
    assert!(wire(&mut s).is_empty());
    feed(&mut s, ":srv CAP me NAK server-time");
    assert_eq!(wire(&mut s), ["CAP END"]);
    assert!(s.cap_enabled("multi-prefix") && !s.cap_enabled("server-time"));
}

#[test]
fn no_caps_wanted_ends_immediately() {
    let mut s = Session::new(Config::new("h", "me"));
    s.start();
    s.take_outbox();
    feed(&mut s, ":srv CAP * LS :nothing-we-want");
    assert_eq!(wire(&mut s), ["CAP END"]);
}

#[test]
fn sasl_flow_and_failure_still_ends_cap() {
    let mut cfg = Config::new("h", "me");
    cfg.sasl = Some(("acct".into(), "pw".into()));
    let mut s = Session::new(cfg.clone());
    s.start();
    s.take_outbox();
    feed(&mut s, ":srv CAP * LS :sasl=EXTERNAL,PLAIN");
    assert_eq!(wire(&mut s), ["CAP REQ sasl"]);
    feed(&mut s, ":srv CAP me ACK sasl");
    assert_eq!(wire(&mut s), ["AUTHENTICATE PLAIN"]);
    feed(&mut s, "AUTHENTICATE +");
    assert_eq!(wire(&mut s), ["AUTHENTICATE AGFjY3QAcHc="]);
    feed(&mut s, ":srv 904 me :SASL authentication failed");
    assert_eq!(wire(&mut s), ["CAP END"]);
    let ev = s.take_events();
    assert!(ev.iter().any(|e| matches!(e, Event::SaslResult { ok: false, .. })));

    // Server without PLAIN: don't request sasl, never send credentials.
    let mut s = Session::new(cfg);
    s.start();
    s.take_outbox();
    feed(&mut s, ":srv CAP * LS :sasl=EXTERNAL");
    assert_eq!(wire(&mut s), ["CAP END"]);
}

#[test]
fn long_sasl_payload_is_chunked_with_terminator() {
    let mut cfg = Config::new("h", "me");
    cfg.sasl = Some(("a".into(), "p".repeat(500)));
    let mut s = Session::new(cfg);
    s.start();
    s.take_outbox();
    feed(&mut s, ":srv CAP * LS :sasl");
    feed(&mut s, ":srv CAP me ACK sasl");
    s.take_outbox();
    feed(&mut s, "AUTHENTICATE +");
    let out = wire(&mut s);
    assert!(out.len() >= 2);
    assert_eq!(out[0].len(), "AUTHENTICATE ".len() + 400);
    assert!(out.last().unwrap().len() <= "AUTHENTICATE ".len() + 400);
}

#[test]
fn ping_pong_and_nick_fallback() {
    let mut s = Session::new(Config::new("h", "me"));
    s.start();
    s.take_outbox();
    feed(&mut s, "PING :token123");
    assert_eq!(wire(&mut s), ["PONG token123"]);
    feed(&mut s, ":srv 433 * me :Nickname is already in use");
    assert_eq!(wire(&mut s), ["NICK me_"]);
    feed(&mut s, ":srv 001 me_ :Welcome");
    assert_eq!(s.nick(), "me_");
    assert!(s.is_registered());
}

#[test]
fn names_join_mode_nick_quit_tracking() {
    let mut s = registered(Config::new("h", "me"));
    feed(&mut s, ":me!u@h JOIN #Chan");
    feed(&mut s, ":srv 353 me = #chan :@Op +voiced plain");
    feed(&mut s, ":srv 366 me #chan :End of /NAMES");
    let ch = s.channel("#CHAN").expect("case-insensitive lookup");
    assert!(ch.names_complete);
    assert_eq!(ch.members.len(), 4);
    assert_eq!(ch.members["op"].modes, "o");

    feed(&mut s, ":Op!u@h MODE #chan +v-o plain Op");
    // +v on plain; -o on Op: argument order is plain then Op
    let ch = s.channel("#chan").unwrap();
    assert_eq!(ch.members["plain"].modes, "v");
    assert_eq!(ch.members["op"].modes, "");

    feed(&mut s, ":plain!u@h NICK Renamed");
    assert!(s.channel("#chan").unwrap().members.contains_key("renamed"));
    assert!(!s.channel("#chan").unwrap().members.contains_key("plain"));

    s.take_events();
    feed(&mut s, ":Renamed!u@h QUIT :bye");
    let ev = s.take_events();
    assert!(matches!(&ev[0], Event::Quit { channels, .. } if channels == &["#Chan".to_owned()]));

    feed(&mut s, ":me!u@h PART #chan");
    assert!(s.channel("#chan").is_none());
}

#[test]
fn own_nick_change_and_kick_of_self() {
    let mut s = registered(Config::new("h", "me"));
    feed(&mut s, ":me!u@h JOIN #c");
    feed(&mut s, ":me!u@h NICK newme");
    assert_eq!(s.nick(), "newme");
    feed(&mut s, ":op!u@h KICK #c newme :out");
    assert!(s.channel("#c").is_none());
}

#[test]
fn mode_argument_rules_follow_chanmodes() {
    let s = registered(Config::new("h", "me"));
    // k always takes an arg, l only when set, b is a list (arg), n none.
    let c = s.parse_modes("+kl-kl+nb", &["key".into(), "5".into(), "key".into(), "*!*@bad".into()]);
    let got: Vec<(bool, char, Option<&str>)> =
        c.iter().map(|m| (m.adding, m.mode, m.arg.as_deref())).collect();
    assert_eq!(
        got,
        [
            (true, 'k', Some("key")),
            (true, 'l', Some("5")),
            (false, 'k', Some("key")),
            (false, 'l', None),
            (true, 'n', None),
            (true, 'b', Some("*!*@bad")),
        ]
    );
}

#[test]
fn chat_action_ctcp_and_autoreply() {
    let mut s = registered(Config::new("h", "me"));
    feed(&mut s, "@time=2024-05-05T10:00:00.000Z :a!u@h PRIVMSG #c :hello");
    feed(&mut s, ":a!u@h PRIVMSG #c :\x01ACTION dances\x01");
    feed(&mut s, ":a!u@h PRIVMSG me :\x01VERSION\x01");
    feed(&mut s, ":a!u@h PRIVMSG me :\x01PING 12345\x01");
    feed(&mut s, ":a!u@h NOTICE me :\x01VERSION other client\x01"); // reply: must not trigger a reply
    let out = wire(&mut s);
    assert_eq!(out.len(), 2);
    assert!(out[0].starts_with("NOTICE a :\x01VERSION ChimeraIRC "));
    assert_eq!(out[1], "NOTICE a :\x01PING 12345\x01");

    let ev = s.take_events();
    assert!(matches!(&ev[0], Event::Chat { kind: ChatKind::Message, time: Some(t), .. } if t.starts_with("2024")));
    assert!(matches!(&ev[1], Event::Chat { kind: ChatKind::Action, text, .. } if text == "dances"));
    assert!(matches!(&ev[4], Event::Ctcp { reply: true, .. }));
}

#[test]
fn extended_join_account_and_away() {
    let mut s = registered(Config::new("h", "me"));
    feed(&mut s, ":me!u@h JOIN #c * :Me Real");
    feed(&mut s, ":x!u@h JOIN #c acct :Real Name");
    feed(&mut s, ":x!u@h AWAY :gone");
    let ev = s.take_events();
    assert!(matches!(&ev[1], Event::Join { account: Some(a), realname: Some(r), .. } if a == "acct" && r == "Real Name"));
    assert!(matches!(&ev[0], Event::Join { account: None, .. }));
    assert!(matches!(&ev[2], Event::Away { message: Some(_), .. }));
}

#[test]
fn members_snapshot_is_coalesced_sorted_and_only_after_names_done() {
    let mut s = registered(Config::new("h", "me"));
    feed(&mut s, ":me!u@h JOIN #c");
    feed(&mut s, ":srv 353 me = #c :+voice @Op zed alice");
    assert!(!s.take_events().iter().any(|e| matches!(e, Event::Members { .. })));
    feed(&mut s, ":srv 366 me #c :End");
    feed(&mut s, ":new!u@h JOIN #c");
    feed(&mut s, ":zed!u@h QUIT");
    let members: Vec<_> = s
        .take_events()
        .into_iter()
        .filter_map(|e| match e {
            Event::Members { members, .. } => Some(members),
            _ => None,
        })
        .collect();
    assert_eq!(members.len(), 1, "one snapshot per drain");
    let names: Vec<String> = members[0].iter().map(|(p, n)| format!("{p}{n}")).collect();
    assert_eq!(names, ["@Op", "+voice", "alice", "me", "new"]);
}

#[test]
fn list_reply_becomes_events() {
    let mut s = registered(Config::new("h", "me"));
    feed(&mut s, ":srv 321 me Channel :Users  Name");
    feed(&mut s, ":srv 322 me #rust 1234 :[+nt] The Rust language");
    feed(&mut s, ":srv 322 me #empty 3");
    feed(&mut s, ":srv 322 me #bad notanumber :topic");
    feed(&mut s, ":srv 323 me :End of /LIST");
    let ev = s.take_events();
    assert_eq!(ev[0], Event::ListStart);
    assert_eq!(ev[1], Event::ListEntry { channel: "#rust".into(), users: 1234, topic: "[+nt] The Rust language".into() });
    assert_eq!(ev[2], Event::ListEntry { channel: "#empty".into(), users: 3, topic: String::new() });
    assert_eq!(ev[3], Event::ListEntry { channel: "#bad".into(), users: 0, topic: "topic".into() });
    assert_eq!(ev[4], Event::ListEnd);
    assert_eq!(ev.len(), 5, "list numerics must not also appear as generic numerics");
}

#[test]
fn ignored_users_are_dropped_with_no_events_and_no_ctcp_reply() {
    use chimerairc_core::IgnoreRule;
    let mut cfg = Config::new("irc.example.net", "me");
    let mut troll = IgnoreRule::new("*!*@evil.host");
    troll.notices = false; // notices still allowed through
    cfg.ignores = vec![troll, {
        let mut r = IgnoreRule::new("spammer");
        r.channels = vec!["#only-here".into()];
        r
    }];
    let mut s = registered(cfg);

    feed(&mut s, ":bad!u@evil.host PRIVMSG #c :hello");
    feed(&mut s, ":bad!u@evil.host PRIVMSG me :\x01ACTION waves\x01");
    feed(&mut s, ":bad!u@evil.host PRIVMSG me :\x01VERSION\x01");
    feed(&mut s, ":bad!u@evil.host PRIVMSG me :\x01PING 1\x01");
    feed(&mut s, ":good!u@fine.host PRIVMSG #c :hi");
    assert!(wire(&mut s).iter().all(|l| !l.contains("VERSION") && !l.contains("PING")), "no auto-replies to ignored users");
    let ev = s.take_events();
    assert_eq!(ev.len(), 1, "only the good user's message survives: {ev:?}");
    assert!(matches!(&ev[0], Event::Chat { source, .. } if source.name == "good"));

    // notices were left unignored for this rule
    feed(&mut s, ":bad!u@evil.host NOTICE me :psst");
    assert!(matches!(s.take_events().as_slice(), [Event::Chat { kind: ChatKind::Notice, .. }]));

    // a channel-restricted rule only applies in that channel
    feed(&mut s, ":spammer!u@h PRIVMSG #elsewhere :buy now");
    feed(&mut s, ":spammer!u@h PRIVMSG #only-here :buy now");
    feed(&mut s, ":spammer!u@h PRIVMSG me :buy now (private)");
    let ev = s.take_events();
    assert_eq!(ev.len(), 2, "{ev:?}");

    // rules can be replaced live; an empty list lets everything through again
    s.set_ignores(Vec::new());
    feed(&mut s, ":bad!u@evil.host PRIVMSG #c :hello again");
    assert_eq!(s.take_events().len(), 1);
}

#[test]
fn own_echoed_messages_and_servers_are_never_ignored() {
    use chimerairc_core::IgnoreRule;
    let mut cfg = Config::new("h", "me");
    cfg.ignores = vec![IgnoreRule::new("*")]; // catch-all
    let mut s = registered(cfg);
    feed(&mut s, ":me!u@h PRIVMSG #c :my own line (echo-message)");
    feed(&mut s, ":irc.server NOTICE me :server notice");
    feed(&mut s, ":someone!u@h PRIVMSG #c :silenced");
    let ev = s.take_events();
    assert_eq!(ev.len(), 2, "{ev:?}");
}


// ---- CTCP TIME and the reply rate limit ---------------------------------------------------------

static FAKE_NOW_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn fake_now() -> std::time::Duration {
    std::time::Duration::from_millis(FAKE_NOW_MS.load(std::sync::atomic::Ordering::SeqCst))
}

fn fixed_time() -> String {
    "Wed Nov 18 11:06:40 2026 UTC".to_owned()
}

#[test]
fn ctcp_time_is_answered_with_the_configured_time_text() {
    let mut cfg = Config::new("h", "me");
    cfg.time_text = fixed_time;
    let mut s = registered(cfg);
    feed(&mut s, ":a!u@h PRIVMSG me :\x01TIME\x01");
    feed(&mut s, ":a!u@h NOTICE me :\x01TIME Mon Jan  1 00:00:00 2024\x01"); // someone's reply: no answer
    let out = wire(&mut s);
    assert_eq!(out, ["NOTICE a :\x01TIME Wed Nov 18 11:06:40 2026 UTC\x01"]);
}

#[test]
fn ctcp_time_defaults_to_the_current_utc_time() {
    let mut s = registered(Config::new("h", "me"));
    feed(&mut s, ":a!u@h PRIVMSG me :\x01TIME\x01");
    let out = wire(&mut s);
    assert_eq!(out.len(), 1);
    assert!(out[0].starts_with("NOTICE a :\x01TIME ") && out[0].ends_with(" UTC\x01"), "{}", out[0]);
}

#[test]
fn automatic_ctcp_replies_are_rate_limited_and_recover() {
    let mut cfg = Config::new("h", "me");
    cfg.monotonic = fake_now;
    cfg.ctcp_reply_limit = (3, std::time::Duration::from_secs(10));
    let mut s = registered(cfg);
    FAKE_NOW_MS.store(1_000, std::sync::atomic::Ordering::SeqCst);
    for i in 0..8 {
        feed(&mut s, &format!(":flood{i}!u@h PRIVMSG me :\x01VERSION\x01"));
    }
    let out = wire(&mut s);
    assert_eq!(out.len(), 3, "only the first three of eight requests are answered: {out:?}");
    // every request is still reported to the interface
    let ctcp_events = s.take_events().iter().filter(|e| matches!(e, Event::Ctcp { .. })).count();
    assert_eq!(ctcp_events, 8);
    // still inside the window: still silent
    FAKE_NOW_MS.store(9_000, std::sync::atomic::Ordering::SeqCst);
    feed(&mut s, ":late!u@h PRIVMSG me :\x01PING 1\x01");
    assert!(wire(&mut s).is_empty());
    // after the window has passed, replies resume
    FAKE_NOW_MS.store(11_500, std::sync::atomic::Ordering::SeqCst);
    feed(&mut s, ":later!u@h PRIVMSG me :\x01PING 2\x01");
    assert_eq!(wire(&mut s), ["NOTICE later :\x01PING 2\x01"]);
}

#[test]
fn a_zero_limit_turns_automatic_ctcp_replies_off() {
    let mut cfg = Config::new("h", "me");
    cfg.ctcp_reply_limit = (0, std::time::Duration::from_secs(10));
    let mut s = registered(cfg);
    feed(&mut s, ":a!u@h PRIVMSG me :\x01VERSION\x01");
    assert!(wire(&mut s).is_empty());
}

#[test]
fn a_netsplit_batch_becomes_one_summary_and_still_updates_the_member_list() {
    let mut s = registered(Config::new("h", "me"));
    feed(&mut s, ":me!m@h JOIN #c");
    feed(&mut s, ":srv 353 me = #c :me bob carol");
    feed(&mut s, ":srv 366 me #c :End");
    s.take_events();
    feed(&mut s, ":srv BATCH +ns netsplit a.example b.example");
    feed(&mut s, "@batch=ns :bob!b@h QUIT :a.example b.example");
    feed(&mut s, "@batch=ns :carol!c@h QUIT :a.example b.example");
    feed(&mut s, "@batch=ns :bob!b@h QUIT :again");
    let during = s.take_events();
    assert!(!during.iter().any(|e| matches!(e, Event::Quit { .. })), "quits inside the batch are folded: {during:?}");
    feed(&mut s, ":srv BATCH -ns");
    let ev = s.take_events();
    let split = ev.iter().find_map(|e| match e {
        Event::NetSplit { servers, nicks, .. } => Some((servers.clone(), nicks.clone())),
        _ => None,
    });
    assert_eq!(split, Some((vec!["a.example".to_string(), "b.example".to_string()], vec!["bob".to_string(), "carol".to_string()])));
    // the channel's member list no longer has them
    let members = ev.iter().chain(during.iter()).filter_map(|e| match e {
        Event::Members { members, .. } => Some(members.clone()),
        _ => None,
    });
    assert_eq!(members.last().map(|m| m.len()), Some(1));
}

#[test]
fn a_netjoin_batch_becomes_one_summary_and_quits_outside_a_batch_are_unchanged() {
    let mut s = registered(Config::new("h", "me"));
    feed(&mut s, ":me!m@h JOIN #c");
    s.take_events();
    feed(&mut s, ":srv BATCH +nj netjoin a.example b.example");
    feed(&mut s, "@batch=nj :bob!b@h JOIN #c");
    feed(&mut s, "@batch=nj :carol!c@h JOIN #c");
    feed(&mut s, ":srv BATCH -nj");
    let ev = s.take_events();
    assert!(!ev.iter().any(|e| matches!(e, Event::Join { .. })), "{ev:?}");
    assert!(ev.iter().any(|e| matches!(e, Event::NetJoin { nicks, .. } if nicks == &["bob", "carol"])), "{ev:?}");
    // an ordinary quit still reports normally
    feed(&mut s, ":bob!b@h QUIT :bye");
    assert!(s.take_events().iter().any(|e| matches!(e, Event::Quit { nick, .. } if nick == "bob")));
    // a batch tag that names no batch we know does not hide a join
    feed(&mut s, "@batch=ghost :dave!d@h JOIN #c");
    assert!(s.take_events().iter().any(|e| matches!(e, Event::Join { nick, .. } if nick == "dave")));
}

// ------------------------------------------------------------------------------- typing and setname

fn caps_session(extra: &str, isupport: &str) -> Session {
    let mut s = Session::new(Config::new("h", "me"));
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
    feed(&mut s, &format!(":srv 005 me PREFIX=(ov)@+ CHANTYPES=# {isupport} :are supported"));
    s.take_events();
    s
}

#[test]
fn setname_is_requested_and_reported() {
    let mut s = Session::new(Config::new("h", "me"));
    s.start();
    s.take_outbox();
    feed(&mut s, ":srv CAP * LS :setname away-notify");
    assert!(wire(&mut s).contains(&"CAP REQ setname".to_owned()));
    let mut s = caps_session("setname", "");
    feed(&mut s, ":bob!b@h SETNAME :Bob the Builder");
    assert!(s.take_events().iter().any(|e| matches!(e, Event::SetName { nick, realname } if nick == "bob" && realname == "Bob the Builder")));
}

#[test]
fn typing_tags_from_others_become_events_but_not_our_own_or_ignored_ones() {
    use chimerairc_core::TypingState;
    let mut s = caps_session("message-tags", "");
    feed(&mut s, "@+typing=active :bob!b@h TAGMSG #c");
    feed(&mut s, "@+typing=paused :bob!b@h TAGMSG #c");
    feed(&mut s, "@+typing=done :bob!b@h TAGMSG me");
    feed(&mut s, "@+typing=bogus :bob!b@h TAGMSG #c");
    feed(&mut s, "@+draft/react=x :bob!b@h TAGMSG #c");
    feed(&mut s, "@+typing=active :me!m@h TAGMSG #c");
    let got: Vec<_> = s.take_events().into_iter().filter_map(|e| match e {
        Event::Typing { nick, target, state } => Some((nick, target, state)),
        _ => None,
    }).collect();
    assert_eq!(got, vec![
        ("bob".to_string(), "#c".to_string(), TypingState::Active),
        ("bob".to_string(), "#c".to_string(), TypingState::Paused),
        ("bob".to_string(), "me".to_string(), TypingState::Done),
    ]);
}

#[test]
fn our_typing_is_sent_only_when_the_server_allows_it() {
    use chimerairc_core::TypingState;
    let mut s = caps_session("message-tags", "");
    s.typing("#c", TypingState::Active);
    assert_eq!(wire(&mut s), ["@+typing=active TAGMSG #c"]);
    s.typing("bob", TypingState::Done);
    assert_eq!(wire(&mut s), ["@+typing=done TAGMSG bob"]);
    // the server forbids client tags
    let mut s = caps_session("message-tags", "CLIENTTAGDENY=*");
    s.typing("#c", TypingState::Active);
    assert!(wire(&mut s).is_empty());
    // forbids all but typing
    let mut s = caps_session("message-tags", "CLIENTTAGDENY=*,-typing");
    s.typing("#c", TypingState::Paused);
    assert_eq!(wire(&mut s), ["@+typing=paused TAGMSG #c"]);
    // no message-tags: nothing
    let mut s = caps_session("away-notify", "");
    s.typing("#c", TypingState::Active);
    assert!(wire(&mut s).is_empty());
}

#[test]
fn alternate_nicks_are_tried_in_order_then_the_underscore_fallback() {
    let mut cfg = Config::new("h", "me");
    cfg.alt_nicks = vec!["me2".into(), "me3".into()];
    let mut s = Session::new(cfg);
    s.start();
    s.take_outbox();
    feed(&mut s, ":srv 433 * me :Nickname is already in use");
    assert_eq!(wire(&mut s), ["NICK me2"]);
    feed(&mut s, ":srv 433 * me2 :Nickname is already in use");
    assert_eq!(wire(&mut s), ["NICK me3"]);
    feed(&mut s, ":srv 433 * me3 :Nickname is already in use");
    assert_eq!(wire(&mut s), ["NICK me3_"], "the list is used up: the old fallback");
    feed(&mut s, ":srv 001 me3_ :Welcome");
    assert_eq!(s.nick(), "me3_");
}
