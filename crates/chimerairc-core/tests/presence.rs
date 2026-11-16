//! Watching nicknames: IRCv3 MONITOR where the server has it, ISON polling where it does not.

use std::time::Duration;
use chimerairc_core::{Config, Event, Session};
use chimerairc_proto::Message;

fn feed(s: &mut Session, line: &str) {
    s.handle(&Message::parse(line).unwrap());
}

fn wire(s: &mut Session) -> Vec<String> {
    s.take_outbox().iter().map(|m| m.encode_wire().unwrap()).collect()
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// A registered session whose MOTD has ended, on a server with `isupport` tokens.
fn ready(isupport: &str) -> Session {
    let mut s = Session::new(Config::new("h", "me"));
    s.start();
    feed(&mut s, ":srv CAP * LS :multi-prefix");
    feed(&mut s, ":srv CAP me ACK multi-prefix");
    feed(&mut s, ":srv 001 me :Welcome");
    feed(&mut s, &format!(":srv 005 me {isupport} :are supported by this server"));
    s.take_outbox();
    s.take_events();
    s
}

fn presence(s: &mut Session) -> Vec<(String, bool, bool)> {
    s.take_events()
        .into_iter()
        .filter_map(|e| match e {
            Event::Presence { nick, online, announce, .. } => Some((nick, online, announce)),
            _ => None,
        })
        .collect()
}

#[test]
fn monitor_is_used_when_the_server_has_it_and_only_after_the_motd() {
    let mut s = ready("MONITOR=100 NETWORK=x");
    s.set_watch(&names(&["Alice", "bob"]));
    assert!(wire(&mut s).is_empty(), "nothing is sent before the server is ready");
    feed(&mut s, ":srv 376 me :End of MOTD");
    assert_eq!(wire(&mut s), ["MONITOR + Alice,bob"]);
    feed(&mut s, ":srv 730 me :alice!a@host.example");
    feed(&mut s, ":srv 731 me :bob");
    assert_eq!(presence(&mut s), [("Alice".into(), true, false), ("bob".into(), false, false)]);
    // later changes are announced
    feed(&mut s, ":srv 731 me :ALICE");
    feed(&mut s, ":srv 730 me :bob!b@h");
    assert_eq!(presence(&mut s), [("Alice".into(), false, true), ("bob".into(), true, true)]);
    // the same state again is not reported twice
    feed(&mut s, ":srv 730 me :bob!b@h");
    assert!(presence(&mut s).is_empty());
}

#[test]
fn the_mask_is_passed_on_and_strangers_are_ignored() {
    let mut s = ready("MONITOR");
    s.set_watch(&names(&["alice"]));
    feed(&mut s, ":srv 376 me :End");
    s.take_outbox();
    feed(&mut s, ":srv 730 me :alice!al@host.example,stranger!x@y");
    let ev = s.take_events();
    assert!(ev.iter().any(|e| matches!(e, Event::Presence { nick, mask: Some(m), .. } if nick == "alice" && m == "alice!al@host.example")), "{ev:?}");
    assert_eq!(ev.iter().filter(|e| matches!(e, Event::Presence { .. })).count(), 1);
    assert!(!ev.iter().any(|e| matches!(e, Event::Numeric { code: 730, .. })), "swallowed, not shown as text");
}

#[test]
fn changing_the_list_clears_and_resends_and_removed_names_are_forgotten() {
    let mut s = ready("MONITOR=100");
    feed(&mut s, ":srv 376 me :End");
    s.set_watch(&names(&["a", "b"]));
    assert_eq!(wire(&mut s), ["MONITOR + a,b"]);
    feed(&mut s, ":srv 730 me :a!u@h,b!u@h");
    s.take_events();
    s.set_watch(&names(&["b", "c"]));
    assert_eq!(wire(&mut s), ["MONITOR C", "MONITOR + b,c"]);
    // `a` is no longer watched: a late report about it shows nothing
    feed(&mut s, ":srv 731 me :a");
    assert!(presence(&mut s).is_empty());
    // `b` was online before and still is: its state survived the change
    feed(&mut s, ":srv 730 me :b!u@h");
    assert!(presence(&mut s).is_empty());
}

#[test]
fn bad_and_duplicate_names_are_dropped() {
    let mut s = ready("MONITOR");
    feed(&mut s, ":srv 376 me :End");
    s.set_watch(&names(&["ok", "OK", "#chan", "a b", "x,y", "", "fine"]));
    assert_eq!(wire(&mut s), ["MONITOR + ok,fine"]);
}

#[test]
fn names_beyond_the_monitor_limit_are_polled_instead() {
    let mut s = ready("MONITOR=2");
    feed(&mut s, ":srv 376 me :End");
    s.set_watch(&names(&["a", "b", "c"]));
    assert_eq!(wire(&mut s), ["MONITOR + a,b"]);
    s.poll_at(Duration::from_secs(100));
    assert_eq!(wire(&mut s), ["ISON c"]);
    feed(&mut s, ":srv 303 me :c");
    assert_eq!(presence(&mut s), [("c".into(), true, false)]);
}

#[test]
fn a_full_monitor_list_moves_the_refused_names_to_polling() {
    let mut s = ready("MONITOR");
    feed(&mut s, ":srv 376 me :End");
    s.set_watch(&names(&["a", "b"]));
    s.take_outbox();
    feed(&mut s, ":srv 734 me 1 b :Monitor list is full");
    s.poll_at(Duration::from_secs(5));
    assert_eq!(wire(&mut s), ["ISON b"]);
}

#[test]
fn without_monitor_ison_is_polled_once_a_minute() {
    let mut s = ready("NETWORK=x");
    s.set_watch(&names(&["Alice", "bob"]));
    s.poll_at(Duration::from_secs(1));
    assert!(wire(&mut s).is_empty(), "not before the MOTD");
    feed(&mut s, ":srv 376 me :End");
    assert!(wire(&mut s).is_empty(), "no MONITOR here");
    s.poll_at(Duration::from_secs(10));
    assert_eq!(wire(&mut s), ["ISON :Alice bob"]);
    // another poll waits for the answer
    s.poll_at(Duration::from_secs(200));
    assert!(wire(&mut s).is_empty());
    feed(&mut s, ":srv 303 me :alice");
    assert_eq!(presence(&mut s), [("Alice".into(), true, false), ("bob".into(), false, false)]);
    // not again within a minute, then again
    s.poll_at(Duration::from_secs(69));
    assert!(wire(&mut s).is_empty());
    s.poll_at(Duration::from_secs(70));
    assert_eq!(wire(&mut s), ["ISON :Alice bob"]);
    feed(&mut s, ":srv 303 me :alice bob");
    assert_eq!(presence(&mut s), [("bob".into(), true, true)]);
}

#[test]
fn an_unasked_303_is_shown_as_ordinary_text() {
    let mut s = ready("NETWORK=x");
    feed(&mut s, ":srv 303 me :someone");
    assert!(s.take_events().iter().any(|e| matches!(e, Event::Numeric { code: 303, .. })));
}

#[test]
fn nothing_is_sent_with_an_empty_list() {
    let mut s = ready("MONITOR");
    feed(&mut s, ":srv 376 me :End");
    assert!(wire(&mut s).is_empty());
    s.poll_at(Duration::from_secs(500));
    assert!(wire(&mut s).is_empty());
}

#[test]
fn a_long_list_is_split_over_several_lines() {
    let mut s = ready("MONITOR");
    feed(&mut s, ":srv 376 me :End");
    let many: Vec<String> = (0..60).map(|i| format!("buddy_number_{i:03}")).collect();
    s.set_watch(&many);
    let lines = wire(&mut s);
    assert!(lines.len() > 1, "{lines:?}");
    assert!(lines.iter().all(|l| l.len() < 400 && l.starts_with("MONITOR + ")));
}
