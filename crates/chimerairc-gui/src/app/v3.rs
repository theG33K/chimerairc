//! The program's side of invitations, labeled replies, message replies and reactions. The bookkeeping is in `ircv3.rs`.

use super::*;
use crate::ircv3;
use crate::script::Val;

/// The lower-case name that identifies a window's conversation: the channel, or the other person.
pub(super) fn conversation_key(target: &str, other_party: &str) -> String {
    if is_channel(target) { target.to_lowercase() } else { other_party.to_lowercase() }
}

/// Learn what the server can do.
pub(super) fn caps(conn: u32, enabled: &[String]) {
    with(|a| {
        if let Some(c) = a.conns.get_mut(&conn) {
            c.v3.labeled = enabled.iter().any(|e| e == "labeled-response");
            c.v3.tags = enabled.iter().any(|e| e == "message-tags");
        }
    });
}

/// The events that follow answer the command with this label (or, with `None`, no longer do).
pub(super) fn label(conn: u32, label: Option<String>) {
    with(|a| {
        if let Some(c) = a.conns.get_mut(&conn) {
            c.v3.set_scope(label);
        }
    });
}

/// The window a numeric reply should go to: where the command was typed, if the server labels its answers.
pub(super) fn answer_window(conn: u32) -> Option<isize> {
    let h = with(|a| a.conns.get(&conn).and_then(|c| c.v3.scope_window()))?;
    ui::child(hw(h)).is_some().then_some(h)
}

/// Send a command typed in window `h`; where the server labels its answers they come back to that window.
pub(super) fn send_from(h: HWND, conn: u32, m: Message) {
    let sent = with(|a| {
        let c = a.conns.get_mut(&conn)?;
        if !c.v3.labeled {
            return None;
        }
        let label = c.v3.new_label(raw(h));
        Some(c.client.as_ref()?.send_labeled(m.clone(), &label))
    });
    if sent.is_none() {
        send(conn, m);
    }
}

/// Remember a message so it can be replied to; returns the text that goes before the message (the reply context)
/// when it is a reply.
pub(super) fn note_chat(conn: u32, key: &str, nick: &str, text: &str, msgid: Option<&str>, reply_to: Option<&str>) -> Option<String> {
    with(|a| {
        let c = a.conns.get_mut(&conn)?;
        let ctx = reply_to.map(|r| ircv3::reply_context(c.v3.find(key, r)));
        if let Some(id) = msgid {
            c.v3.remember(key, id, nick, text);
        }
        ctx
    })
}

/// `INVITE`: someone invited us, or (with `invite-notify`) someone else to a channel we are in.
pub(super) fn on_invite(conn: u32, me: &str, by: &str, nick: &str, channel: &str) {
    if eq_ic(by, me) {
        return; // our own invitation: the server's confirmation (341) tells us
    }
    let mine = eq_ic(nick, me);
    let (line, win) = if mine {
        (format!("* {by} has invited you to {channel} (type /join {channel} to accept)"), None)
    } else {
        (format!("* {by} has invited {nick} to {channel}"), window_for(conn, Kind::Channel, channel, false))
    };
    match win {
        Some(h) => print(h, &line),
        None => server_print(conn, &line),
    }
    if mine {
        play_sound("invite", "");
    }
    let fields = [("nick", Val::from(by)), ("invitee", Val::from(nick)), ("channel", Val::from(channel)), ("mine", Val::from(mine))];
    scripts::event(conn, win, channel, "invite", &fields);
}

/// A reaction was added (or taken back).
pub(super) fn on_react(conn: u32, me: &str, nick: &str, target: &str, msgid: &str, emoji: &str, remove: bool) {
    let mine = eq_ic(nick, me);
    // in a private chat the conversation is with the other person
    let other = if mine { target } else { nick };
    let key = conversation_key(target, other);
    let original = with(|a| a.conns.get(&conn).and_then(|c| c.v3.find(&key, msgid).cloned()));
    let line = ircv3::reaction_line(nick, mine, emoji, remove, original.as_ref());
    let win = if is_channel(target) { window_for(conn, Kind::Channel, target, false) } else { window_for(conn, Kind::Query, other, false) };
    match win {
        Some(h) => print_k(h, LineKind::Info, &line),
        None => server_print(conn, &format!("[{target}] {}", line.trim_start_matches("* "))),
    }
    let fields = [
        ("nick", Val::from(nick)),
        ("target", Val::from(target)),
        ("emoji", Val::from(emoji)),
        ("msgid", Val::from(msgid)),
        ("remove", Val::from(remove)),
        ("mine", Val::from(mine)),
    ];
    scripts::event(conn, win, target, "react", &fields);
}

fn conversation(h: HWND) -> Option<(Kind, u32, String)> {
    ui::child(h).map(|c| (c.kind, c.conn, c.name.clone())).filter(|(k, _, _)| matches!(k, Kind::Channel | Kind::Query))
}

fn echo_and_nick(conn: u32) -> (bool, String, bool) {
    with(|a| a.conns.get(&conn).map(|c| (c.echo, c.nick.clone(), c.client.is_some())).unwrap_or((false, String::new(), false)))
}

/// `/reply <nick> <text>`: answer that person's latest message in this window.
pub(super) fn reply_command(h: HWND, rest: &str) {
    let Some((_, conn, name)) = conversation(h) else {
        local(h, "* /reply works in a channel or a private chat window.");
        return;
    };
    let Some((nick, text)) = ircv3::split_nick_text(rest) else {
        local(h, "* Usage: /reply <nick> <text>   (answers that person's latest message in this window)");
        return;
    };
    let (echo, me, up) = echo_and_nick(conn);
    if !up {
        local(h, "* Not connected.");
        return;
    }
    let key = name.to_lowercase();
    let original = with(|a| a.conns.get(&conn).and_then(|c| c.v3.last_from(&key, nick).cloned()));
    let Some(original) = original else {
        local(h, &format!("* No recent message from {nick} with an id in this window; sending it as an ordinary message."));
        send_chat(h, conn, &name, "PRIVMSG", &format!("{nick}: {text}"));
        return;
    };
    let mut parts = split_text(text, 400).into_iter();
    let first = parts.next().unwrap_or(text);
    with(|a| a.conns.get(&conn).and_then(|c| c.client.as_ref()).map(|c| c.reply(&name, &original.msgid, first)));
    if !echo {
        let line = format!("<{me}> {} {first}", ircv3::reply_context(Some(&original)));
        print_k(raw(h), LineKind::Normal, &line);
    }
    for part in parts {
        send_chat(h, conn, &name, "PRIVMSG", part);
    }
}

/// `/react <nick> <emoji>` (or `/unreact`): react to that person's latest message in this window.
pub(super) fn react_command(h: HWND, rest: &str, remove: bool) {
    let verb = if remove { "unreact" } else { "react" };
    let Some((_, conn, name)) = conversation(h) else {
        local(h, &format!("* /{verb} works in a channel or a private chat window."));
        return;
    };
    let Some((nick, emoji)) = ircv3::split_nick_text(rest) else {
        local(h, &format!("* Usage: /{verb} <nick> <emoji>   (the reaction goes to that person's latest message in this window)"));
        return;
    };
    let (echo, me, up) = echo_and_nick(conn);
    if !up {
        local(h, "* Not connected.");
        return;
    }
    if !with(|a| a.conns.get(&conn).is_some_and(|c| c.v3.tags)) {
        local(h, "* This server does not support reactions (it needs the message-tags capability).");
        return;
    }
    let key = name.to_lowercase();
    let original = with(|a| a.conns.get(&conn).and_then(|c| c.v3.last_from(&key, nick).cloned()));
    let Some(original) = original else {
        local(h, &format!("* No recent message from {nick} with an id in this window to react to."));
        return;
    };
    let emoji = emoji.split_whitespace().next().unwrap_or(emoji);
    with(|a| a.conns.get(&conn).and_then(|c| c.client.as_ref()).map(|c| c.react(&name, &original.msgid, emoji, remove)));
    if !echo {
        print_k(raw(h), LineKind::Info, &ircv3::reaction_line(&me, true, emoji, remove, Some(&original)));
    }
}

/// `/invite <nick> [#channel]`: the channel is the one of this window when not given.
pub(super) fn invite_command(h: HWND, kind: Kind, conn: u32, name: &str, rest: &str) {
    let mut words = rest.split_whitespace();
    let (Some(nick), channel) = (words.next(), words.next()) else {
        local(h, "* Usage: /invite <nick> [#channel]");
        return;
    };
    let channel = match channel {
        Some(c) => c.to_owned(),
        None if kind == Kind::Channel => name.to_owned(),
        None => {
            local(h, "* Say which channel: /invite <nick> #channel");
            return;
        }
    };
    if !with(|a| a.conns.get(&conn).is_some_and(|c| c.client.is_some())) {
        local(h, "* Not connected. Use /server <host>.");
        return;
    }
    send_from(h, conn, Message::new("INVITE", &[nick, &channel]));
}
