//! Ignore rules, applied inside the session so ignored users also get no automatic replies.

use chimerairc_proto::mask;
use chimerairc_proto::Source;

/// What kind of message is being checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IgnoreKind {
    /// PRIVMSG text and actions (`/me`).
    Message,
    /// Plain NOTICEs.
    Notice,
    /// CTCP requests and replies other than ACTION.
    Ctcp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IgnoreRule {
    /// Full `nick!user@host` mask with `*` / `?` wildcards (see [`mask::normalize`]).
    pub mask: String,
    /// Only on this server (host name); `None` = every server.
    pub network: Option<String>,
    /// Only for messages sent to one of these channels; empty = everywhere, private messages included.
    pub channels: Vec<String>,
    pub messages: bool,
    pub notices: bool,
    pub ctcp: bool,
}

impl IgnoreRule {
    /// Ignore everything from whoever `mask` (a nick or a mask) matches, on every server.
    pub fn new(mask_or_nick: &str) -> IgnoreRule {
        IgnoreRule {
            mask: mask::normalize(mask_or_nick),
            network: None,
            channels: Vec::new(),
            messages: true,
            notices: true,
            ctcp: true,
        }
    }

    /// Does this rule silence a `kind` message from `src` sent to `target` on server `host`?
    pub fn applies(&self, host: &str, src: &Source, target: &str, kind: IgnoreKind) -> bool {
        let kind_on = match kind {
            IgnoreKind::Message => self.messages,
            IgnoreKind::Notice => self.notices,
            IgnoreKind::Ctcp => self.ctcp,
        };
        kind_on
            && self.network.as_deref().is_none_or(|n| n.eq_ignore_ascii_case(host))
            && (self.channels.is_empty() || self.channels.iter().any(|c| c.eq_ignore_ascii_case(target)))
            && mask::matches_source(&self.mask, src)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(s: &str) -> Source {
        Source::parse(s)
    }

    #[test]
    fn kinds_network_and_channels_all_restrict() {
        let mut r = IgnoreRule::new("troll");
        let troll = src("Troll!t@host");
        assert!(r.applies("h", &troll, "#c", IgnoreKind::Message));
        assert!(r.applies("h", &troll, "me", IgnoreKind::Notice));
        r.notices = false;
        assert!(!r.applies("h", &troll, "#c", IgnoreKind::Notice));
        assert!(r.applies("h", &troll, "#c", IgnoreKind::Message));

        r.network = Some("Irc.Example.Net".into());
        assert!(!r.applies("other.net", &troll, "#c", IgnoreKind::Message));
        assert!(r.applies("irc.example.net", &troll, "#c", IgnoreKind::Message));

        r.channels = vec!["#Noisy".into()];
        assert!(r.applies("irc.example.net", &troll, "#noisy", IgnoreKind::Message));
        assert!(!r.applies("irc.example.net", &troll, "#quiet", IgnoreKind::Message));
        assert!(!r.applies("irc.example.net", &troll, "me", IgnoreKind::Message), "private messages are not in the list");
    }

    #[test]
    fn masks_and_servers() {
        let r = IgnoreRule::new("*!*@*.evil.example");
        assert!(r.applies("h", &src("a!b@x.evil.example"), "#c", IgnoreKind::Message));
        assert!(!r.applies("h", &src("a!b@good.example"), "#c", IgnoreKind::Message));
        assert!(!IgnoreRule::new("*").applies("h", &src("irc.example.net"), "me", IgnoreKind::Notice));
    }
}
