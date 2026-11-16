//! IRCv3 Strict Transport Security: the value of the `sts` capability.
//!
//! * Over a plaintext connection a server that has `port=<n>` is saying "talk to me over TLS on that
//!   port instead": the client must reconnect securely before sending anything private.
//! * Over a TLS connection `duration=<seconds>` says how long the client must remember to use TLS for
//!   this host (`duration=0` withdraws the policy), and `preload` that the server wants to be in
//!   preload lists.
//!
//! Only parsing lives here; the session decides what to do with it.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Sts {
    pub port: Option<u16>,
    pub duration: Option<u64>,
    pub preload: bool,
}

/// Parse `duration=2592000,port=6697,preload`. Unknown keys are ignored (the spec says so, so new
/// keys can be added later); a malformed `port` or `duration` is treated as absent rather than
/// trusted, and port 0 is never valid.
pub fn parse(value: &str) -> Sts {
    let mut sts = Sts::default();
    for item in value.split(',') {
        let (key, val) = item.split_once('=').map_or((item, None), |(k, v)| (k, Some(v)));
        match (key, val) {
            ("port", Some(v)) => sts.port = v.parse::<u16>().ok().filter(|p| *p != 0),
            ("duration", Some(v)) => sts.duration = v.parse::<u64>().ok(),
            ("preload", _) => sts.preload = true,
            _ => {}
        }
    }
    sts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_values_servers_send() {
        assert_eq!(parse("duration=2592000,port=6697"), Sts { port: Some(6697), duration: Some(2_592_000), preload: false });
        assert_eq!(parse("port=6697"), Sts { port: Some(6697), duration: None, preload: false });
        assert_eq!(parse("duration=0"), Sts { port: None, duration: Some(0), preload: false }, "0 withdraws the policy");
        assert_eq!(parse("duration=31536000,preload"), Sts { port: None, duration: Some(31_536_000), preload: true });
        assert_eq!(parse(""), Sts::default());
    }

    #[test]
    fn bad_values_are_treated_as_absent_and_unknown_keys_ignored() {
        assert_eq!(parse("port=0").port, None);
        assert_eq!(parse("port=70000").port, None);
        assert_eq!(parse("port=abc").port, None);
        assert_eq!(parse("port=-1").port, None);
        assert_eq!(parse("duration=forever").duration, None);
        assert_eq!(parse("duration=-5").duration, None);
        assert_eq!(parse("duration=99999999999999999999999").duration, None);
        assert_eq!(parse("port").port, None, "a key without a value");
        assert_eq!(parse("future-key=x,port=6697,another"), Sts { port: Some(6697), duration: None, preload: false });
        // keys are case-sensitive, as in the spec
        assert_eq!(parse("PORT=6697,Duration=5"), Sts::default());
    }
}
