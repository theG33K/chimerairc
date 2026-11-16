//! IRCv3 typing indicators (the `+typing` client tag): when to tell others we are typing, and who is typing to
//! us. Pure state with the time passed in (milliseconds), so it is tested without a window or a clock.

use chimerairc_core::TypingState;

/// While we keep typing, say so again after this long (the other end forgets after about six seconds).
pub const RESEND_MS: u64 = 3000;
/// No key for this long: we have paused.
pub const PAUSE_MS: u64 = 5000;
/// How long someone's `active` counts, unless they say `paused` or `done` first.
pub const ACTIVE_FOR_MS: u64 = 6000;

/// Milliseconds since the program started, for the state machines below.
pub fn now_ms() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

/// What we last told the other end about our entry box.
#[derive(Default, Debug)]
pub struct Outgoing {
    sent: Option<TypingState>,
    last_sent: u64,
    last_key: u64,
}

/// Whether the entry text is something the others would see as a message: not empty, and not a `/command`.
pub fn is_message_text(text: &str) -> bool {
    let t = text.trim_start();
    !t.is_empty() && !(t.starts_with('/') && !t.starts_with("//"))
}

impl Outgoing {
    /// The entry box changed. Returns what to send, if anything.
    pub fn edited(&mut self, now: u64, text: &str) -> Option<TypingState> {
        if !is_message_text(text) {
            return self.finish();
        }
        self.last_key = now;
        let due = match self.sent {
            Some(TypingState::Active) => now.saturating_sub(self.last_sent) >= RESEND_MS,
            _ => true,
        };
        if due {
            self.sent = Some(TypingState::Active);
            self.last_sent = now;
            return Some(TypingState::Active);
        }
        None
    }

    /// Called about once a second: after a pause in typing, say `paused`.
    pub fn tick(&mut self, now: u64) -> Option<TypingState> {
        if self.sent == Some(TypingState::Active) && now.saturating_sub(self.last_key) >= PAUSE_MS {
            self.sent = Some(TypingState::Paused);
            self.last_sent = now;
            return Some(TypingState::Paused);
        }
        None
    }

    /// The text was sent or cleared (or the window closes): `done`, but only if we had said we were typing.
    pub fn finish(&mut self) -> Option<TypingState> {
        match self.sent.take() {
            Some(TypingState::Active) | Some(TypingState::Paused) => Some(TypingState::Done),
            _ => None,
        }
    }
}

/// Who is typing to us in one window.
#[derive(Default, Debug)]
pub struct Incoming {
    /// (nick, lower-case key, active until)
    active: Vec<(String, String, u64)>,
}

/// More people than this typing at once is not worth listing, and bounds the memory a flood can use.
const MAX_TRACKED: usize = 16;

impl Incoming {
    /// Note what `nick` said about their typing. Returns true if the list of typing people changed.
    pub fn set(&mut self, now: u64, nick: &str, state: TypingState) -> bool {
        let key = nick.to_lowercase();
        let pos = self.active.iter().position(|(_, k, _)| *k == key);
        match (state, pos) {
            (TypingState::Active, Some(i)) => {
                self.active[i].2 = now + ACTIVE_FOR_MS;
                false
            }
            (TypingState::Active, None) => {
                if self.active.len() >= MAX_TRACKED {
                    return false;
                }
                self.active.push((nick.to_owned(), key, now + ACTIVE_FOR_MS));
                true
            }
            (_, Some(i)) => {
                self.active.remove(i);
                true
            }
            (_, None) => false,
        }
    }

    /// Forget those whose `active` ran out. Returns true if someone was dropped.
    pub fn expire(&mut self, now: u64) -> bool {
        let before = self.active.len();
        self.active.retain(|(_, _, until)| *until > now);
        self.active.len() != before
    }

    /// Someone who spoke is no longer typing.
    pub fn spoke(&mut self, nick: &str) -> bool {
        let key = nick.to_lowercase();
        let before = self.active.len();
        self.active.retain(|(_, k, _)| *k != key);
        self.active.len() != before
    }

    /// `alice is typing...`, `alice and bob are typing...`, `alice, bob and 2 others are typing...`, or `""`.
    pub fn text(&self) -> String {
        let names: Vec<&str> = self.active.iter().map(|(n, _, _)| n.as_str()).collect();
        match names.as_slice() {
            [] => String::new(),
            [a] => format!("{a} is typing..."),
            [a, b] => format!("{a} and {b} are typing..."),
            [a, b, rest @ ..] => format!("{a}, {b} and {} other{} are typing...", rest.len(), if rest.len() == 1 { "" } else { "s" }),
        }
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TypingState::*;

    #[test]
    fn only_real_message_text_counts_as_typing() {
        assert!(is_message_text("hello"));
        assert!(is_message_text("  hi"));
        assert!(is_message_text("//escaped slash"));
        assert!(!is_message_text(""));
        assert!(!is_message_text("   "));
        assert!(!is_message_text("/join #c"));
    }

    #[test]
    fn typing_is_announced_repeated_paused_and_ended() {
        let mut o = Outgoing::default();
        assert_eq!(o.edited(0, "h"), Some(Active));
        assert_eq!(o.edited(500, "he"), None, "no need to repeat so soon");
        assert_eq!(o.edited(3100, "hel"), Some(Active), "refreshed while still typing");
        assert_eq!(o.tick(5000), None);
        assert_eq!(o.tick(3100 + PAUSE_MS), Some(Paused));
        assert_eq!(o.tick(3100 + PAUSE_MS + 1000), None, "paused only once");
        assert_eq!(o.edited(20_000, "hello"), Some(Active), "typing again after a pause");
        assert_eq!(o.edited(20_100, ""), Some(Done), "clearing the box ends it");
        assert_eq!(o.edited(20_200, ""), None);
    }

    #[test]
    fn commands_and_sent_text_do_not_leave_a_typing_state_behind() {
        let mut o = Outgoing::default();
        assert_eq!(o.edited(0, "/jo"), None);
        assert_eq!(o.edited(10, "hi"), Some(Active));
        assert_eq!(o.edited(20, "/hi"), Some(Done), "became a command");
        assert_eq!(o.edited(30, "again"), Some(Active));
        assert_eq!(o.finish(), Some(Done), "sent");
        assert_eq!(o.finish(), None);
        // paused then sent still says done
        let mut o = Outgoing::default();
        o.edited(0, "x");
        o.tick(PAUSE_MS);
        assert_eq!(o.finish(), Some(Done));
    }

    #[test]
    fn people_typing_to_us_come_and_go() {
        let mut i = Incoming::default();
        assert!(i.set(0, "Alice", Active));
        assert!(!i.set(1000, "alice", Active), "the same person again changes nothing visible");
        assert_eq!(i.text(), "Alice is typing...");
        assert!(i.set(1000, "bob", Active));
        assert_eq!(i.text(), "Alice and bob are typing...");
        assert!(i.set(1000, "carol", Active) && i.set(1000, "dave", Active));
        assert_eq!(i.text(), "Alice, bob and 2 others are typing...");
        assert!(i.set(2000, "ALICE", Done));
        assert!(i.set(2000, "bob", Paused));
        assert_eq!(i.text(), "carol and dave are typing...");
        assert!(!i.set(2000, "zed", Done), "done from someone not listed");
    }

    #[test]
    fn active_runs_out_and_speaking_ends_it() {
        let mut i = Incoming::default();
        i.set(0, "alice", Active);
        i.set(4000, "bob", Active);
        assert!(!i.expire(5999));
        assert!(i.expire(6000), "alice's six seconds are up");
        assert_eq!(i.text(), "bob is typing...");
        assert!(i.spoke("BOB"));
        assert!(i.is_empty() && i.text().is_empty());
        assert!(!i.spoke("bob"));
    }

    #[test]
    fn a_flood_of_typists_is_bounded() {
        let mut i = Incoming::default();
        for n in 0..100 {
            i.set(0, &format!("n{n}"), Active);
        }
        assert_eq!(i.active.len(), MAX_TRACKED);
    }
}
