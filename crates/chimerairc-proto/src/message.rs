use crate::prefix::Source;
use std::fmt;

/// Max line length excluding tags, including CRLF (Modern IRC / RFC 2812).
pub const MAX_LINE_LEN: usize = 512;
/// Max size of the tags section including `@` and trailing space (IRCv3 message-tags).
pub const MAX_TAGS_LEN: usize = 8191;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub key: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub tags: Vec<Tag>,
    pub source: Option<Source>,
    /// Command word or 3-digit numeric, upper-cased.
    pub command: String,
    pub params: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    MissingCommand,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Empty => f.write_str("empty message"),
            ParseError::MissingCommand => f.write_str("message has no command"),
        }
    }
}
impl std::error::Error for ParseError {}

fn unescape_tag(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some(':') => out.push(';'),
            Some('s') => out.push(' '),
            Some('\\') => out.push('\\'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other), // unknown escape: drop the backslash
            None => {}                      // lone trailing backslash is dropped
        }
    }
    out
}

fn escape_tag(v: &str, out: &mut String) {
    for c in v.chars() {
        match c {
            ';' => out.push_str("\\:"),
            ' ' => out.push_str("\\s"),
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
}

impl Message {
    pub fn new(command: &str, params: &[&str]) -> Message {
        Message {
            tags: Vec::new(),
            source: None,
            command: command.to_ascii_uppercase(),
            params: params.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// Parse one line. Trailing CR/LF is stripped; the line must not contain
    /// embedded newlines.
    pub fn parse(line: &str) -> Result<Message, ParseError> {
        let mut rest = line.trim_end_matches(['\r', '\n']);
        rest = rest.trim_start_matches(' ');
        if rest.is_empty() {
            return Err(ParseError::Empty);
        }

        let mut tags = Vec::new();
        if let Some(r) = rest.strip_prefix('@') {
            let (raw, after) = r.split_once(' ').unwrap_or((r, ""));
            for item in raw.split(';').filter(|s| !s.is_empty()) {
                let (key, value) = match item.split_once('=') {
                    Some((k, v)) if !v.is_empty() => (k, Some(unescape_tag(v))),
                    Some((k, _)) => (k, None),
                    None => (item, None),
                };
                tags.push(Tag { key: key.to_owned(), value });
            }
            rest = after.trim_start_matches(' ');
        }

        let mut source = None;
        if let Some(r) = rest.strip_prefix(':') {
            let (src, after) = r.split_once(' ').unwrap_or((r, ""));
            source = Some(Source::parse(src));
            rest = after.trim_start_matches(' ');
        }

        if rest.is_empty() {
            return Err(ParseError::MissingCommand);
        }
        let (command, mut rest) = rest.split_once(' ').unwrap_or((rest, ""));
        let mut params = Vec::new();
        loop {
            rest = rest.trim_start_matches(' ');
            if rest.is_empty() {
                break;
            }
            if let Some(trailing) = rest.strip_prefix(':') {
                params.push(trailing.to_owned());
                break;
            }
            let (p, after) = rest.split_once(' ').unwrap_or((rest, ""));
            params.push(p.to_owned());
            rest = after;
        }

        Ok(Message { tags, source, command: command.to_ascii_uppercase(), params })
    }

    pub fn tag(&self, key: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.key == key)
            .map(|t| t.value.as_deref().unwrap_or(""))
    }

    /// True for a three-digit numeric reply.
    pub fn is_numeric(&self) -> bool {
        self.command.len() == 3 && self.command.bytes().all(|b| b.is_ascii_digit())
    }

    /// Serialize without trailing CRLF. The last parameter is sent as trailing
    /// when it is empty, contains a space, or starts with ':'.
    pub fn encode(&self) -> String {
        let mut out = String::new();
        if !self.tags.is_empty() {
            out.push('@');
            for (i, t) in self.tags.iter().enumerate() {
                if i > 0 {
                    out.push(';');
                }
                out.push_str(&t.key);
                if let Some(v) = &t.value {
                    out.push('=');
                    escape_tag(v, &mut out);
                }
            }
            out.push(' ');
        }
        if let Some(s) = &self.source {
            out.push(':');
            out.push_str(&s.to_string());
            out.push(' ');
        }
        out.push_str(&self.command);
        let n = self.params.len();
        for (i, p) in self.params.iter().enumerate() {
            out.push(' ');
            if i + 1 == n && (p.is_empty() || p.contains(' ') || p.starts_with(':')) {
                out.push(':');
            }
            out.push_str(p);
        }
        out
    }
}

/// Why a message cannot be put on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    /// CR, LF or NUL in a field (would allow command injection).
    ControlChar,
    /// Empty command, or a non-final parameter that is empty, has a space, or starts with ':'.
    InvalidField,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::ControlChar => f.write_str("CR, LF or NUL in message"),
            EncodeError::InvalidField => f.write_str("invalid command or parameter"),
        }
    }
}
impl std::error::Error for EncodeError {}

impl Message {
    /// Validate and serialize for sending (no CRLF). Rejects fields that would
    /// corrupt framing, and truncates the non-tag part to fit [`MAX_LINE_LEN`]
    /// (510 bytes before CRLF) on a UTF-8 boundary.
    pub fn encode_wire(&self) -> Result<String, EncodeError> {
        let bad = |s: &str| s.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0));
        if self.command.is_empty() || self.command.contains(' ') {
            return Err(EncodeError::InvalidField);
        }
        let src = self.source.as_ref().map(|s| s.to_string());
        if bad(&self.command)
            || self.params.iter().any(|p| bad(p))
            || src.as_deref().is_some_and(bad)
            || self.tags.iter().any(|t| bad(&t.key) || t.value.as_deref().is_some_and(bad))
        {
            return Err(EncodeError::ControlChar);
        }
        if let Some((_, init)) = self.params.split_last() {
            if init.iter().any(|p| p.is_empty() || p.contains(' ') || p.starts_with(':')) {
                return Err(EncodeError::InvalidField);
            }
        }
        let mut body = Message { tags: Vec::new(), ..self.clone() }.encode();
        let limit = MAX_LINE_LEN - 2;
        if body.len() > limit {
            let mut cut = limit;
            while !body.is_char_boundary(cut) {
                cut -= 1;
            }
            body.truncate(cut);
        }
        if self.tags.is_empty() {
            return Ok(body);
        }
        let full = self.encode();
        let tag_part = &full[..full.len() - Message { tags: Vec::new(), ..self.clone() }.encode().len()];
        Ok(format!("{tag_part}{body}"))
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encode())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_privmsg() {
        let m = Message::parse(":nick!user@host PRIVMSG #chan :hello world\r\n").unwrap();
        let s = m.source.as_ref().unwrap();
        assert_eq!((s.name.as_str(), s.user.as_deref(), s.host.as_deref()), ("nick", Some("user"), Some("host")));
        assert_eq!(m.command, "PRIVMSG");
        assert_eq!(m.params, ["#chan", "hello world"]);
    }

    #[test]
    fn tags_and_escapes() {
        let m = Message::parse("@time=2024-01-01T00:00:00.000Z;+x=a\\:b\\sc;flag;empty= :srv NOTICE * :hi").unwrap();
        assert_eq!(m.tag("time"), Some("2024-01-01T00:00:00.000Z"));
        assert_eq!(m.tag("+x"), Some("a;b c"));
        assert_eq!(m.tag("flag"), Some(""));
        assert_eq!(m.tag("empty"), Some(""));
        assert_eq!(m.tag("missing"), None);
    }

    #[test]
    fn numeric_and_no_params() {
        let m = Message::parse(":irc.example.net 001 me :Welcome").unwrap();
        assert!(m.is_numeric());
        assert!(m.source.unwrap().is_server());
        let p = Message::parse("PING").unwrap();
        assert!(p.params.is_empty());
    }

    #[test]
    fn extra_spaces_and_empty_trailing() {
        let m = Message::parse("MODE   #c   +o   nick  :").unwrap();
        assert_eq!(m.params, ["#c", "+o", "nick", ""]);
    }

    #[test]
    fn errors() {
        assert_eq!(Message::parse("\r\n"), Err(ParseError::Empty));
        assert_eq!(Message::parse(":only.prefix"), Err(ParseError::MissingCommand));
        assert_eq!(Message::parse("@a=b"), Err(ParseError::MissingCommand));
    }

    #[test]
    fn encode_roundtrip() {
        let mut m = Message::new("privmsg", &["#c", "two words"]);
        m.tags.push(Tag { key: "+draft/reply".into(), value: Some("a b;c".into()) });
        let wire = m.encode();
        assert_eq!(wire, "@+draft/reply=a\\sb\\:c PRIVMSG #c :two words");
        assert_eq!(Message::parse(&wire).unwrap(), m);
    }

    #[test]
    fn wire_rejects_injection_and_bad_params() {
        assert_eq!(Message::new("PRIVMSG", &["#c", "a\r\nQUIT"]).encode_wire(), Err(EncodeError::ControlChar));
        assert_eq!(Message::new("JOIN", &["#c d", "k"]).encode_wire(), Err(EncodeError::InvalidField));
        assert_eq!(Message::new("MODE", &["", "+i"]).encode_wire(), Err(EncodeError::InvalidField));
    }

    #[test]
    fn wire_truncates_on_char_boundary_keeping_tags() {
        let text = "é".repeat(400); // 800 bytes
        let mut m = Message::new("PRIVMSG", &["#c", &text]);
        m.tags.push(Tag { key: "+a".into(), value: Some("b".into()) });
        let w = m.encode_wire().unwrap();
        let (tags, body) = w.split_once(' ').unwrap();
        assert_eq!(tags, "@+a=b");
        assert!(body.len() <= 510);
        assert!(body.starts_with("PRIVMSG #c é"));
        assert!(body.is_char_boundary(body.len())); // never split mid-codepoint
        assert!(body.chars().all(|c| c.is_ascii() || c == 'é'));
    }

    #[test]
    fn encode_colon_leading_and_empty() {
        assert_eq!(Message::new("PRIVMSG", &["#c", ":)"]).encode(), "PRIVMSG #c ::)");
        assert_eq!(Message::new("TOPIC", &["#c", ""]).encode(), "TOPIC #c :");
        assert_eq!(Message::new("JOIN", &["#c"]).encode(), "JOIN #c");
    }
}
