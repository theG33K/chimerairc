use chimerairc_proto::{Ctcp, Source};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatKind {
    Message,
    Notice,
    Action,
}

/// What the other end of a `+typing` tag says about their typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingState {
    /// They are typing now (good for a few seconds).
    Active,
    /// They typed something and stopped.
    Paused,
    /// They sent it or cleared the box.
    Done,
}

impl TypingState {
    /// The value of the `+typing` tag.
    pub fn tag_value(self) -> &'static str {
        match self {
            TypingState::Active => "active",
            TypingState::Paused => "paused",
            TypingState::Done => "done",
        }
    }
    pub fn from_tag_value(v: &str) -> Option<TypingState> {
        match v {
            "active" => Some(TypingState::Active),
            "paused" => Some(TypingState::Paused),
            "done" => Some(TypingState::Done),
            _ => None,
        }
    }
}

/// One parsed channel-mode change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeChange {
    pub adding: bool,
    pub mode: char,
    pub arg: Option<String>,
}

/// One line of channel history delivered in a `chathistory` batch.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryLine {
    /// The IRCv3 `server-time` of the line (UTC, `2024-05-05T10:00:00.000Z`).
    pub time: Option<String>,
    pub source: Source,
    pub text: String,
    pub kind: ChatKind,
    /// The `msgid` tag, which lets the UI skip lines it has already shown.
    pub msgid: Option<String>,
}

/// Things the UI cares about. Produced by [`crate::Session`] and delivered by the network layer.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Transport is up (TCP + TLS if enabled); registration is starting.
    Connected,
    /// Server accepted registration (001). `nick` is our confirmed nick.
    Registered { nick: String },
    /// Capability negotiation finished; `enabled` is the final set.
    CapsEnabled { enabled: Vec<String> },
    SaslResult { ok: bool, text: String },
    /// The server (over TLS) published a Strict Transport Security policy: use TLS for this host for
    /// the next `duration` seconds (0 withdraws the policy). `port` is the port this connection used.
    StsPolicy { duration: u64, port: u16, preload: bool },
    /// The server (over plaintext) asks us to reconnect with TLS on `port`. The connection ends right
    /// after this event, before anything private is sent.
    StsUpgrade { port: u16 },
    /// PRIVMSG / NOTICE / ACTION. `target` is a channel or our own nick.
    /// `time` is the IRCv3 `server-time` tag when present.
    /// `reply_to` is the `msgid` this message answers (the `+draft/reply` tag).
    Chat { source: Source, target: String, text: String, kind: ChatKind, time: Option<String>, msgid: Option<String>, reply_to: Option<String> },
    /// A complete `chathistory` batch for `target`, oldest line first (possibly empty).
    History { target: String, lines: Vec<HistoryLine> },
    /// Non-ACTION CTCP. `reply` is true when it arrived in a NOTICE.
    Ctcp { source: Source, target: String, ctcp: Ctcp, reply: bool },
    Join { channel: String, nick: String, account: Option<String>, realname: Option<String> },
    Part { channel: String, nick: String, reason: Option<String> },
    Quit { nick: String, reason: Option<String>, channels: Vec<String> },
    Kick { channel: String, by: String, victim: String, reason: Option<String> },
    Nick { old: String, new: String },
    /// `setname`: `nick` changed their real name.
    SetName { nick: String, realname: String },
    /// `invite-notify` (or an invitation to us): `by` invited `nick` to `channel`.
    Invite { by: String, nick: String, channel: String },
    /// A reaction (`+draft/react`, or `+draft/unreact` when `remove`) from `nick` to the message `msgid` in `target`.
    React { nick: String, target: String, msgid: String, emoji: String, remove: bool },
    /// `labeled-response`: the events that follow, up to the next `Label { label: None }`, answer the command we
    /// sent with this label.
    Label { label: Option<String> },
    /// `+typing` client tag (a `TAGMSG`): `nick` is typing in `target` (a channel, or our own nick for a private chat).
    Typing { nick: String, target: String, state: TypingState },
    /// A `netsplit` batch ended: `nicks` quit because the link between `servers` broke (the individual
    /// quits are not reported). Capped at a few thousand names.
    NetSplit { servers: Vec<String>, nicks: Vec<String>, channels: Vec<String> },
    /// A `netjoin` batch ended: `nicks` came back when the link between `servers` was restored.
    NetJoin { servers: Vec<String>, nicks: Vec<String>, channels: Vec<String> },
    Topic { channel: String, topic: Option<String> },
    NamesDone { channel: String },
    /// Full member list of a channel after any change, sorted by rank then nick:
    /// `(prefix symbol or "", nick)`. Replaces any earlier list for that channel.
    Members { channel: String, members: Vec<(String, String)> },
    Mode { target: String, by: Option<String>, changes: Vec<ModeChange> },
    Away { nick: String, message: Option<String> },
    Account { nick: String, account: Option<String> },
    ChgHost { nick: String, user: String, host: String },
    /// A watched nickname (see `Session::set_watch`) came online or went offline. `announce` is true
    /// when this is a change from a state we already knew, false for the first report after a
    /// (re)connect. `mask` is `nick!user@host` when the server said so.
    Presence { nick: String, online: bool, mask: Option<String>, announce: bool },
    /// Start of a `LIST` reply (321).
    ListStart,
    /// One channel from a `LIST` reply (322). `topic` may be empty.
    ListEntry { channel: String, users: u32, topic: String },
    /// End of a `LIST` reply (323).
    ListEnd,
    /// Any other numeric (MOTD lines, WHOIS, errors, ...). `params` excludes our nick.
    Numeric { code: u16, params: Vec<String> },
    /// Server `ERROR` or a local protocol problem.
    Error { text: String },
    Disconnected { reason: String },
}
