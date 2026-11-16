/// A CTCP request/reply carried inside a PRIVMSG/NOTICE trailing parameter
/// (`\x01COMMAND args\x01`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ctcp {
    pub command: String,
    pub args: String,
}

impl Ctcp {
    /// Parse a message body; returns `None` if it is not CTCP-framed.
    /// A missing closing `\x01` is tolerated, as many clients omit it.
    pub fn parse(body: &str) -> Option<Ctcp> {
        let inner = body.strip_prefix('\x01')?;
        let inner = inner.strip_suffix('\x01').unwrap_or(inner);
        let (command, args) = inner.split_once(' ').unwrap_or((inner, ""));
        if command.is_empty() {
            return None;
        }
        Some(Ctcp { command: command.to_ascii_uppercase(), args: args.to_owned() })
    }

    pub fn encode(&self) -> String {
        if self.args.is_empty() {
            format!("\x01{}\x01", self.command)
        } else {
            format!("\x01{} {}\x01", self.command, self.args)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_action() {
        let c = Ctcp::parse("\x01ACTION waves\x01").unwrap();
        assert_eq!(c.command, "ACTION");
        assert_eq!(c.args, "waves");
        assert_eq!(c.encode(), "\x01ACTION waves\x01");
    }

    #[test]
    fn parse_no_args_unterminated() {
        let c = Ctcp::parse("\x01version").unwrap();
        assert_eq!(c.command, "VERSION");
        assert_eq!(c.args, "");
    }

    #[test]
    fn not_ctcp() {
        assert!(Ctcp::parse("hello").is_none());
        assert!(Ctcp::parse("\x01\x01").is_none());
    }
}
