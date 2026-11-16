//! Favorite servers and their channels (the data behind the Favorites menu).
//!
//! Pure logic, no Win32. Passwords and channel keys are deliberately NOT stored: favorites
//! live in a plain-text settings file.

/// A favorite server with the channels to join on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    /// Display name (falls back to the host when empty).
    pub name: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub channels: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Favorites {
    pub servers: Vec<Server>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Added {
    /// Server and channel were both new.
    ServerAndChannel,
    Channel,
    AlreadyThere,
}

/// Single-line text that is safe in the settings file (no `|`, `,` handled separately, no controls).
fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() || c == '|' { ' ' } else { c }).collect::<String>().trim().to_owned()
}

/// A channel name we are willing to store and later send in a JOIN.
pub fn valid_channel(c: &str) -> bool {
    c.len() >= 2
        && c.starts_with(['#', '&', '+', '!'])
        && !c.chars().any(|ch| ch.is_whitespace() || ch.is_control() || ch == ',' || ch == '|' || ch == ':')
}

/// A host name or address we are willing to connect to.
pub fn valid_host(h: &str) -> bool {
    !h.is_empty() && !h.chars().any(|c| c.is_whitespace() || c.is_control() || c == '|' || c == '/' || c == '\\')
}

impl Server {
    pub fn new(name: &str, host: &str, port: u16, tls: bool, channels: &[String]) -> Server {
        let mut s = Server { name: clean(name), host: clean(host), port, tls, channels: Vec::new() };
        for c in channels {
            s.add_channel(c);
        }
        s
    }

    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.host
        } else {
            &self.name
        }
    }

    /// Add a channel unless it is invalid or already there (case-insensitive).
    pub fn add_channel(&mut self, c: &str) -> bool {
        let c = c.trim();
        if !valid_channel(c) || self.channels.iter().any(|x| x.eq_ignore_ascii_case(c)) {
            return false;
        }
        self.channels.push(c.to_owned());
        true
    }

    pub fn same_endpoint(&self, host: &str, port: u16) -> bool {
        self.host.eq_ignore_ascii_case(host) && self.port == port
    }

    /// `name|host|port|T|#a,#b` (`T` for TLS, `P` for plaintext).
    pub fn to_setting(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            clean(&self.name),
            clean(&self.host),
            self.port,
            if self.tls { 'T' } else { 'P' },
            self.channels.join(",")
        )
    }

    pub fn from_setting(s: &str) -> Option<Server> {
        let mut it = s.splitn(5, '|');
        let (name, host, port, tls, chans) = (it.next()?, it.next()?, it.next()?, it.next()?, it.next().unwrap_or(""));
        let port: u16 = port.trim().parse().ok().filter(|p| *p > 0)?;
        if !valid_host(host.trim()) {
            return None;
        }
        let channels: Vec<String> = chans.split(',').map(str::to_owned).collect();
        Some(Server::new(name, host.trim(), port, tls.trim() != "P", &channels))
    }
}

impl Favorites {
    pub fn find(&self, host: &str, port: u16) -> Option<usize> {
        self.servers.iter().position(|s| s.same_endpoint(host, port))
    }

    /// Add a whole server; false if that host:port is already a favorite.
    pub fn add_server(&mut self, s: Server) -> bool {
        if self.find(&s.host, s.port).is_some() {
            return false;
        }
        self.servers.push(s);
        true
    }

    /// Add a channel to the favorite for `host:port`, creating that server favorite if needed.
    pub fn add_channel(&mut self, host: &str, port: u16, tls: bool, channel: &str) -> Added {
        if !valid_channel(channel.trim()) {
            return Added::AlreadyThere;
        }
        match self.find(host, port) {
            Some(i) => {
                if self.servers[i].add_channel(channel) {
                    Added::Channel
                } else {
                    Added::AlreadyThere
                }
            }
            None => {
                self.servers.push(Server::new("", host, port, tls, &[channel.to_owned()]));
                Added::ServerAndChannel
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chans(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn settings_roundtrip() {
        let s = Server::new("Libera", "irc.libera.chat", 6697, true, &chans(&["#rust", "#linux"]));
        assert_eq!(s.to_setting(), "Libera|irc.libera.chat|6697|T|#rust,#linux");
        assert_eq!(Server::from_setting(&s.to_setting()), Some(s));
        let p = Server::new("", "localhost", 6667, false, &[]);
        assert_eq!(Server::from_setting(&p.to_setting()), Some(p));
    }

    #[test]
    fn rejects_bad_lines_and_filters_bad_channels() {
        for bad in ["", "x", "a|b", "n||6697|T|", "n|h|0|T|", "n|h|99999|T|", "n|h|abc|T|", "n|bad host|6697|T|"] {
            assert_eq!(Server::from_setting(bad), None, "{bad:?}");
        }
        let s = Server::from_setting("n|h|6667|T|#ok, #ok ,notachannel,#a b,#x:y,##double,#").unwrap();
        // case-insensitive duplicates and invalid names are dropped; "#" alone is too short
        assert_eq!(s.channels, ["#ok", "##double"]);
    }

    #[test]
    fn names_cannot_inject_fields_or_lines() {
        let s = Server::new("evil|name\nnext", "h", 6667, true, &[]);
        assert!(!s.name.contains(['|', '\n']));
        let line = s.to_setting();
        assert_eq!(line.matches('|').count(), 4);
        assert!(!line.contains('\n'));
    }

    #[test]
    fn add_channel_creates_dedupes_and_matches_host_case_insensitively() {
        let mut f = Favorites::default();
        assert_eq!(f.add_channel("Irc.Example.Net", 6697, true, "#Rust"), Added::ServerAndChannel);
        assert_eq!(f.add_channel("irc.example.net", 6697, true, "#rust"), Added::AlreadyThere);
        assert_eq!(f.add_channel("irc.example.net", 6697, true, "#other"), Added::Channel);
        // same host, different port or TLS endpoint is a separate favorite
        assert_eq!(f.add_channel("irc.example.net", 6667, false, "#rust"), Added::ServerAndChannel);
        assert_eq!(f.servers.len(), 2);
        assert_eq!(f.servers[0].channels, ["#Rust", "#other"]);
        assert_eq!(f.add_channel("h", 1, true, "not-a-channel"), Added::AlreadyThere);
        assert_eq!(f.servers.len(), 2);
    }

    #[test]
    fn add_server_refuses_duplicates() {
        let mut f = Favorites::default();
        assert!(f.add_server(Server::new("A", "h", 6697, true, &[])));
        assert!(!f.add_server(Server::new("B", "H", 6697, true, &[])));
        assert!(f.add_server(Server::new("C", "h", 6667, false, &[])));
    }

    #[test]
    fn labels_and_validators() {
        assert_eq!(Server::new("", "host", 1, true, &[]).label(), "host");
        assert_eq!(Server::new("Nice", "host", 1, true, &[]).label(), "Nice");
        assert!(valid_channel("#a") && valid_channel("&local") && valid_channel("!x1"));
        assert!(!valid_channel("#") && !valid_channel("a#") && !valid_channel("#a,b") && !valid_channel("#a b"));
        assert!(valid_host("irc.example.net") && valid_host("::1") && valid_host("127.0.0.1"));
        assert!(!valid_host("") && !valid_host("a b") && !valid_host("a/b") && !valid_host("a|b"));
    }
}
