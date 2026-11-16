//! The server list: well-known networks that come with ChimeraIRC, plus servers the user adds.
//!
//! Pure data (no Win32), unit-tested. The built-in entries are the networks' public round-robin
//! names as best known when this was written; they are suggestions, not promises, so every one
//! can be overridden by adding an entry with the same host and port, and the user's own servers are
//! stored in `settings.ini` (`server=` lines). Passwords are never part of a server entry.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub network: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

/// What [`add`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Added {
    New,
    /// An entry with the same host and port existed and was replaced.
    Replaced,
}

/// A usable host name or address: no spaces, control characters or separators, sensible length.
pub fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '[' | ']'))
}

fn clean_name(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(60).collect()
}

impl Server {
    /// Validated constructor: `None` for a bad host or port. A missing network name becomes "Other".
    pub fn new(network: &str, host: &str, port: u16, tls: bool) -> Option<Server> {
        let host = host.trim();
        if !valid_host(host) || port == 0 {
            return None;
        }
        let network = clean_name(network);
        Some(Server { network: if network.is_empty() { "Other".into() } else { network }, host: host.to_owned(), port, tls })
    }

    pub fn same_endpoint(&self, host: &str, port: u16) -> bool {
        self.host.eq_ignore_ascii_case(host) && self.port == port
    }

    /// `irc.libera.chat:6697 (TLS)`
    pub fn label(&self) -> String {
        format!("{}:{}{}", self.host, self.port, if self.tls { " (TLS)" } else { "" })
    }

    /// One `settings.ini` value: TAB-separated `network host port tls`.
    pub fn to_setting(&self) -> String {
        format!("{}\t{}\t{}\t{}", self.network, self.host, self.port, if self.tls { 1 } else { 0 })
    }

    pub fn from_setting(v: &str) -> Option<Server> {
        let mut p = v.split('\t');
        let (network, host, port, tls) = (p.next()?, p.next()?, p.next()?, p.next()?);
        Server::new(network, host, port.trim().parse().ok()?, tls.trim() == "1")
    }
}

/// The networks that ship with ChimeraIRC (TLS wherever the network offers it).
pub fn defaults() -> Vec<Server> {
    let s = |network: &str, host: &str, port: u16, tls: bool| Server { network: network.into(), host: host.into(), port, tls };
    vec![
        s("Libera.Chat", "irc.libera.chat", 6697, true),
        s("OFTC", "irc.oftc.net", 6697, true),
        s("EFnet", "irc.efnet.org", 6697, true),
        s("Rizon", "irc.rizon.net", 6697, true),
        s("Undernet", "irc.undernet.org", 6697, true),
        s("QuakeNet", "irc.quakenet.org", 6697, true),
        s("DALnet", "irc.dal.net", 6697, true),
        s("IRCnet", "open.ircnet.net", 6667, false),
        s("Snoonet", "irc.snoonet.org", 6697, true),
        s("hackint", "irc.hackint.org", 6697, true),
        s("EsperNet", "irc.esper.net", 6697, true),
        s("SwiftIRC", "irc.swiftirc.net", 6697, true),
        s("GeekShed", "irc.geekshed.net", 6697, true),
    ]
}

pub fn is_builtin(host: &str, port: u16) -> bool {
    defaults().iter().any(|d| d.same_endpoint(host, port))
}

/// Built-in servers and the user's, sorted by network then host. A user entry with the same host
/// and port as a built-in one replaces it.
pub fn merged(user: &[Server]) -> Vec<Server> {
    let mut all: Vec<Server> = defaults().into_iter().filter(|d| !user.iter().any(|u| u.same_endpoint(&d.host, d.port))).collect();
    all.extend(user.iter().cloned());
    all.sort_by_key(|s| (s.network.to_lowercase(), s.host.to_lowercase(), s.port));
    all
}

/// Group a sorted list by network, keeping the order.
pub fn groups(list: &[Server]) -> Vec<(String, Vec<Server>)> {
    let mut out: Vec<(String, Vec<Server>)> = Vec::new();
    for s in list {
        match out.last_mut() {
            Some((n, v)) if n.eq_ignore_ascii_case(&s.network) => v.push(s.clone()),
            _ => out.push((s.network.clone(), vec![s.clone()])),
        }
    }
    out
}

/// Add a server to the user's list, replacing an entry with the same host and port.
pub fn add(user: &mut Vec<Server>, s: Server) -> Added {
    match user.iter_mut().find(|u| u.same_endpoint(&s.host, s.port)) {
        Some(existing) => {
            *existing = s;
            Added::Replaced
        }
        None => {
            user.push(s);
            Added::New
        }
    }
}

/// Remove one of the user's own servers. Built-in ones cannot be removed (override them instead).
pub fn remove(user: &mut Vec<Server>, host: &str, port: u16) -> bool {
    let before = user.len();
    user.retain(|u| !u.same_endpoint(host, port));
    user.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_list_is_valid_and_has_no_duplicates() {
        let d = defaults();
        assert!(d.len() >= 10);
        for s in &d {
            assert!(Server::new(&s.network, &s.host, s.port, s.tls).as_ref() == Some(s), "{s:?} must pass its own validation");
            assert!(s.port >= 1024, "{s:?}");
        }
        for (i, a) in d.iter().enumerate() {
            for b in &d[i + 1..] {
                assert!(!a.same_endpoint(&b.host, b.port), "{} appears twice", a.host);
            }
        }
        // TLS is the default wherever the entry is not plain 6667
        assert!(d.iter().filter(|s| !s.tls).all(|s| s.port == 6667));
        assert!(d.iter().any(|s| s.host == "irc.libera.chat" && s.tls && s.port == 6697));
    }

    #[test]
    fn settings_roundtrip_and_hostile_input_is_rejected() {
        let s = Server::new("My Net", "irc.example.org", 6697, true).unwrap();
        assert_eq!(Server::from_setting(&s.to_setting()), Some(s.clone()));
        assert_eq!(s.label(), "irc.example.org:6697 (TLS)");
        assert_eq!(Server::new("", "h.example", 6667, false).unwrap().network, "Other");
        for bad in [
            Server::new("n", "", 6667, false),
            Server::new("n", "has space", 6667, false),
            Server::new("n", "a\tb", 6667, false),
            Server::new("n", "a\nb", 6667, false),
            Server::new("n", "evil/path", 6667, false),
            Server::new("n", "host", 0, false),
            Server::new("n", &"a".repeat(300), 6667, false),
        ] {
            assert!(bad.is_none());
        }
        // a hostile network name cannot add columns or lines to the settings file
        let evil = Server::new("a\tb\nc", "h.example", 6667, false).unwrap();
        assert!(!evil.network.contains(['\t', '\n']));
        assert_eq!(evil.to_setting().matches('\t').count(), 3);
        // garbage settings lines are ignored, not panics
        for junk in ["", "one", "a\tb", "a\tb\tnotaport\t1", "a\t\t6667\t1", "a\th\t70000\t1"] {
            assert!(Server::from_setting(junk).is_none(), "{junk:?}");
        }
    }

    #[test]
    fn merging_sorts_and_lets_user_entries_override_built_in_ones() {
        let mut user = vec![];
        assert_eq!(add(&mut user, Server::new("Mine", "irc.mine.example", 6667, false).unwrap()), Added::New);
        // same host and port as a built-in entry, different network label and TLS setting
        assert_eq!(add(&mut user, Server::new("Libera (mine)", "IRC.LIBERA.CHAT", 6697, false).unwrap()), Added::New);
        let all = merged(&user);
        assert_eq!(all.iter().filter(|s| s.host.eq_ignore_ascii_case("irc.libera.chat")).count(), 1, "replaced, not duplicated");
        assert!(all.iter().any(|s| s.network == "Libera (mine)" && !s.tls));
        assert!(all.windows(2).all(|w| (w[0].network.to_lowercase(), w[0].host.to_lowercase()) <= (w[1].network.to_lowercase(), w[1].host.to_lowercase())));
        // adding the same endpoint again replaces
        assert_eq!(add(&mut user, Server::new("Mine 2", "irc.mine.example", 6667, true).unwrap()), Added::Replaced);
        assert_eq!(user.len(), 2);
        assert!(user.iter().any(|s| s.network == "Mine 2" && s.tls));
    }

    #[test]
    fn only_the_users_own_servers_can_be_removed_and_networks_group_up() {
        let mut user = vec![Server::new("Mine", "irc.mine.example", 6667, false).unwrap(), Server::new("Mine", "irc2.mine.example", 6667, false).unwrap()];
        assert!(is_builtin("irc.libera.chat", 6697) && !is_builtin("irc.mine.example", 6667));
        assert!(!remove(&mut user, "irc.libera.chat", 6697), "a built-in is not in the user's list");
        assert!(remove(&mut user, "IRC.MINE.EXAMPLE", 6667));
        assert_eq!(user.len(), 1);
        assert!(!remove(&mut user, "irc.mine.example", 6667), "already gone");
        let user = vec![Server::new("Mine", "a.example", 6667, false).unwrap(), Server::new("mine", "b.example", 6667, false).unwrap()];
        let g = groups(&merged(&user));
        let mine: Vec<_> = g.iter().filter(|(n, _)| n.eq_ignore_ascii_case("mine")).collect();
        assert_eq!(mine.len(), 1, "network names group case-insensitively");
        assert_eq!(mine[0].1.len(), 2);
        assert!(g.len() >= defaults().len());
    }
}
