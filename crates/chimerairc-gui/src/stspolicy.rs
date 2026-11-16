//! Remembered Strict Transport Security policies: hosts that promised, over a verified TLS
//! connection, to be reachable by TLS for a while, so that ChimeraIRC never contacts them without it
//! until the promise runs out. Pure data, unit-tested; the settings file stores it.

use crate::servers::valid_host;

/// No promise is kept longer than this, whatever the server says (ten years).
const MAX_SECS: u64 = 10 * 365 * 24 * 3600;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub host: String,
    /// The TLS port to use instead of a plaintext one.
    pub port: u16,
    /// Unix time (seconds) when the policy runs out.
    pub expires: u64,
}

/// What [`set`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    New,
    /// An existing policy got a later expiry (or another port).
    Refreshed,
    Removed,
    /// Nothing to do (withdrawing a policy that was not there, or an invalid host).
    Nothing,
}

impl Policy {
    /// One `settings.ini` value: TAB-separated `host port expires`.
    pub fn to_setting(&self) -> String {
        format!("{}\t{}\t{}", self.host, self.port, self.expires)
    }

    pub fn from_setting(v: &str) -> Option<Policy> {
        let mut p = v.split('\t');
        let (host, port, expires) = (p.next()?.trim(), p.next()?.trim(), p.next()?.trim());
        let port: u16 = port.parse().ok().filter(|p| *p != 0)?;
        let expires: u64 = expires.parse().ok()?;
        valid_host(host).then(|| Policy { host: host.to_ascii_lowercase(), port, expires })
    }
}

/// The live policy for `host` at unix time `now`, if there is one.
pub fn lookup<'a>(list: &'a [Policy], host: &str, now: u64) -> Option<&'a Policy> {
    list.iter().find(|p| p.host.eq_ignore_ascii_case(host) && p.expires > now)
}

/// Record what a server just announced over TLS: `duration` seconds from `now` (0 withdraws it).
pub fn set(list: &mut Vec<Policy>, host: &str, port: u16, duration: u64, now: u64) -> Change {
    if !valid_host(host) || port == 0 {
        return Change::Nothing;
    }
    let host = host.to_ascii_lowercase();
    let existing = list.iter().position(|p| p.host == host);
    if duration == 0 {
        return match existing {
            Some(i) => {
                list.remove(i);
                Change::Removed
            }
            None => Change::Nothing,
        };
    }
    let expires = now.saturating_add(duration.min(MAX_SECS));
    match existing {
        Some(i) => {
            let was_live = list[i].expires > now;
            list[i] = Policy { host, port, expires };
            if was_live {
                Change::Refreshed
            } else {
                Change::New
            }
        }
        None => {
            list.push(Policy { host, port, expires });
            Change::New
        }
    }
}

/// Drop policies that have run out.
pub fn prune(list: &mut Vec<Policy>, now: u64) {
    list.retain(|p| p.expires > now);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_policy_applies_until_it_expires_and_matches_case_insensitively() {
        let mut l = Vec::new();
        assert_eq!(set(&mut l, "IRC.Example.NET", 6697, 3600, 1000), Change::New);
        assert_eq!(lookup(&l, "irc.example.net", 1000).map(|p| p.port), Some(6697));
        assert!(lookup(&l, "IRC.EXAMPLE.NET", 4599).is_some());
        assert!(lookup(&l, "irc.example.net", 4600).is_none(), "expired exactly at the end");
        assert!(lookup(&l, "other.example.net", 1000).is_none());
    }

    #[test]
    fn refreshing_extends_withdrawing_removes_and_nonsense_is_ignored() {
        let mut l = Vec::new();
        set(&mut l, "h.example", 6697, 100, 0);
        assert_eq!(set(&mut l, "h.example", 6697, 500, 50), Change::Refreshed);
        assert_eq!(l[0].expires, 550);
        assert_eq!(l.len(), 1, "one entry per host");
        // after it had lapsed, a new announcement counts as new again
        assert_eq!(set(&mut l, "h.example", 6697, 100, 10_000), Change::New);
        assert_eq!(set(&mut l, "h.example", 6697, 0, 10_000), Change::Removed);
        assert!(l.is_empty());
        assert_eq!(set(&mut l, "h.example", 6697, 0, 10_000), Change::Nothing);
        assert_eq!(set(&mut l, "bad host", 6697, 100, 0), Change::Nothing);
        assert_eq!(set(&mut l, "h.example", 0, 100, 0), Change::Nothing);
        assert!(l.is_empty());
    }

    #[test]
    fn promises_are_capped_and_cannot_overflow() {
        let mut l = Vec::new();
        set(&mut l, "h.example", 6697, u64::MAX, 1_000);
        assert_eq!(l[0].expires, 1_000 + MAX_SECS);
        set(&mut l, "g.example", 6697, 100, u64::MAX - 5);
        assert_eq!(l[1].expires, u64::MAX, "saturates instead of wrapping");
    }

    #[test]
    fn pruning_and_the_settings_format() {
        let mut l = vec![
            Policy { host: "a.example".into(), port: 6697, expires: 100 },
            Policy { host: "b.example".into(), port: 7000, expires: 900 },
        ];
        prune(&mut l, 500);
        assert_eq!(l.len(), 1);
        let p = &l[0];
        assert_eq!(Policy::from_setting(&p.to_setting()).as_ref(), Some(p));
        for junk in ["", "h", "h\t6697", "h\tx\t5", "h\t0\t5", "bad host\t6697\t5", "h\t6697\tsoon", "h\t70000\t5"] {
            assert!(Policy::from_setting(junk).is_none(), "{junk:?}");
        }
        assert_eq!(Policy::from_setting("H.Example\t6697\t5").unwrap().host, "h.example", "hosts are lower-cased");
    }
}
