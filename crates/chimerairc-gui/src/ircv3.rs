//! The bookkeeping behind labeled replies, message replies and reactions. Pure data and text, tested here; the
//! program (`app/v3.rs`) puts it to use.

use std::collections::{HashMap, VecDeque};

/// A message seen in a window that has an id, so it can be answered or reacted to later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recent {
    pub msgid: String,
    pub nick: String,
    pub text: String,
}

/// Most messages kept per window, and most windows kept.
const PER_WINDOW: usize = 300;
const WINDOWS: usize = 128;
/// Most labels remembered at once (a label whose answer never comes is forgotten in time).
const LABELS: usize = 64;

/// What one connection knows.
#[derive(Default)]
pub struct State {
    /// The server does `labeled-response`.
    pub labeled: bool,
    /// The server carries client tags (`message-tags`), which replies and reactions need.
    pub tags: bool,
    next_label: u32,
    /// Label -> the window the command was typed in.
    labels: VecDeque<(String, isize)>,
    /// The label whose answer is being processed now.
    scope: Option<String>,
    /// Lower-case window name -> its recent messages, oldest first.
    recent: HashMap<String, VecDeque<Recent>>,
}

impl State {
    /// A new label for a command typed in `window`.
    pub fn new_label(&mut self, window: isize) -> String {
        self.next_label = self.next_label.wrapping_add(1);
        let label = format!("c{}", self.next_label);
        if self.labels.len() >= LABELS {
            self.labels.pop_front();
        }
        self.labels.push_back((label.clone(), window));
        label
    }

    /// The answer to the command with this label is being processed (`None`: it is over).
    pub fn set_scope(&mut self, label: Option<String>) {
        self.scope = label;
    }

    /// The window the command being answered was typed in.
    pub fn scope_window(&self) -> Option<isize> {
        let l = self.scope.as_ref()?;
        self.labels.iter().rev().find(|(x, _)| x == l).map(|(_, w)| *w)
    }

    /// Remember a message of `window` (lower-case name).
    pub fn remember(&mut self, window: &str, msgid: &str, nick: &str, text: &str) {
        if msgid.is_empty() {
            return;
        }
        if !self.recent.contains_key(window) && self.recent.len() >= WINDOWS {
            if let Some(k) = self.recent.keys().next().cloned() {
                self.recent.remove(&k);
            }
        }
        let q = self.recent.entry(window.to_owned()).or_default();
        if q.len() >= PER_WINDOW {
            q.pop_front();
        }
        q.push_back(Recent { msgid: msgid.to_owned(), nick: nick.to_owned(), text: text.to_owned() });
    }

    /// The newest message from `nick` in `window`.
    pub fn last_from(&self, window: &str, nick: &str) -> Option<&Recent> {
        self.recent.get(window)?.iter().rev().find(|r| r.nick.eq_ignore_ascii_case(nick))
    }

    /// The message with this id in `window`.
    pub fn find(&self, window: &str, msgid: &str) -> Option<&Recent> {
        self.recent.get(window)?.iter().rev().find(|r| r.msgid == msgid)
    }

    /// Forget a window's messages (it was closed).
    pub fn forget(&mut self, window: &str) {
        self.recent.remove(window);
    }
}

/// A short piece of a message to show as context: the first 40 characters, with `...` if there is more.
pub fn snippet(text: &str) -> String {
    let one: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let one = one.trim();
    let mut it = one.chars();
    let head: String = it.by_ref().take(40).collect();
    if it.next().is_some() { format!("{head}...") } else { head }
}

/// What a reply shows before its text: who it answers and what they said.
pub fn reply_context(original: Option<&Recent>) -> String {
    match original {
        Some(r) => format!("(re {}: \"{}\")", r.nick, snippet(&r.text)),
        None => "(in reply to an earlier message)".to_owned(),
    }
}

/// The line shown for a reaction.
pub fn reaction_line(who: &str, me: bool, emoji: &str, remove: bool, original: Option<&Recent>) -> String {
    let subject = if me { "You".to_owned() } else { who.to_owned() };
    let verb = match (me, remove) {
        (true, false) => "reacted",
        (true, true) => "took back your reaction",
        (false, false) => "reacted",
        (false, true) => "took back a reaction",
    };
    let tail = match original {
        Some(r) => format!("{}'s message: \"{}\"", r.nick, snippet(&r.text)),
        None => "an earlier message".to_owned(),
    };
    if remove {
        format!("* {subject} {verb} {emoji} on {tail}")
    } else {
        format!("* {subject} {verb} {emoji} to {tail}")
    }
}

/// `nick text...` -> (nick, text). `None` without both.
pub fn split_nick_text(rest: &str) -> Option<(&str, &str)> {
    let (nick, text) = rest.trim().split_once(char::is_whitespace)?;
    let text = text.trim();
    (!nick.is_empty() && !text.is_empty()).then_some((nick, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_leads_back_to_the_window_that_asked() {
        let mut s = State::default();
        let a = s.new_label(11);
        let b = s.new_label(22);
        assert_ne!(a, b);
        assert_eq!(s.scope_window(), None);
        s.set_scope(Some(b.clone()));
        assert_eq!(s.scope_window(), Some(22));
        s.set_scope(Some(a));
        assert_eq!(s.scope_window(), Some(11));
        s.set_scope(Some("unknown".into()));
        assert_eq!(s.scope_window(), None);
        s.set_scope(None);
        assert_eq!(s.scope_window(), None);
    }

    #[test]
    fn old_labels_are_forgotten_and_the_newest_still_work() {
        let mut s = State::default();
        let first = s.new_label(1);
        for i in 0..LABELS {
            s.new_label(100 + i as isize);
        }
        s.set_scope(Some(first));
        assert_eq!(s.scope_window(), None);
        let last = s.new_label(7);
        s.set_scope(Some(last));
        assert_eq!(s.scope_window(), Some(7));
    }

    #[test]
    fn recent_messages_are_found_by_nick_and_by_id() {
        let mut s = State::default();
        s.remember("#c", "m1", "alice", "hello there");
        s.remember("#c", "m2", "bob", "hi");
        s.remember("#c", "m3", "Alice", "second one");
        s.remember("#c", "", "carol", "no id: not kept");
        assert_eq!(s.last_from("#c", "ALICE").map(|r| r.msgid.as_str()), Some("m3"));
        assert_eq!(s.last_from("#c", "bob").map(|r| r.text.as_str()), Some("hi"));
        assert_eq!(s.last_from("#c", "carol"), None);
        assert_eq!(s.last_from("#other", "bob"), None);
        assert_eq!(s.find("#c", "m1").map(|r| r.nick.as_str()), Some("alice"));
        assert_eq!(s.find("#c", "nope"), None);
        s.forget("#c");
        assert_eq!(s.find("#c", "m1"), None);
    }

    #[test]
    fn memory_is_bounded() {
        let mut s = State::default();
        for i in 0..(PER_WINDOW + 50) {
            s.remember("#c", &format!("m{i}"), "a", "x");
        }
        assert_eq!(s.find("#c", "m0"), None);
        assert!(s.find("#c", &format!("m{}", PER_WINDOW + 49)).is_some());
        for i in 0..(WINDOWS + 10) {
            s.remember(&format!("#w{i}"), "m", "a", "x");
        }
        assert!(s.recent.len() <= WINDOWS);
    }

    #[test]
    fn snippets_are_short_one_line_and_safe_with_any_text() {
        assert_eq!(snippet("short"), "short");
        assert_eq!(snippet("  line\r\nbreak\tand tab "), "line  break and tab");
        let long = "x".repeat(60);
        assert_eq!(snippet(&long), format!("{}...", "x".repeat(40)));
        let wide = "é".repeat(41);
        assert_eq!(snippet(&wide), format!("{}...", "é".repeat(40)));
        assert_eq!(snippet(&"é".repeat(40)), "é".repeat(40));
        assert_eq!(snippet(""), "");
    }

    #[test]
    fn replies_and_reactions_read_well() {
        let orig = Recent { msgid: "m1".into(), nick: "alice".into(), text: "lunch at noon?".into() };
        assert_eq!(reply_context(Some(&orig)), "(re alice: \"lunch at noon?\")");
        assert_eq!(reply_context(None), "(in reply to an earlier message)");
        assert_eq!(reaction_line("bob", false, "👍", false, Some(&orig)), "* bob reacted 👍 to alice's message: \"lunch at noon?\"");
        assert_eq!(reaction_line("me", true, "👍", false, Some(&orig)), "* You reacted 👍 to alice's message: \"lunch at noon?\"");
        assert_eq!(reaction_line("bob", false, "👍", true, None), "* bob took back a reaction 👍 on an earlier message");
        assert_eq!(reaction_line("me", true, "❤", true, Some(&orig)), "* You took back your reaction ❤ on alice's message: \"lunch at noon?\"");
    }

    #[test]
    fn command_arguments_split_into_nick_and_text() {
        assert_eq!(split_nick_text("alice  see you there "), Some(("alice", "see you there")));
        assert_eq!(split_nick_text("alice"), None);
        assert_eq!(split_nick_text("alice   "), None);
        assert_eq!(split_nick_text(""), None);
        assert_eq!(split_nick_text("bob 👍"), Some(("bob", "👍")));
    }
}
