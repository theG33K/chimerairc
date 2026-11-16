//! Identities: the nickname, user name, real name and SASL account that go with one network (or one server).
//! Pure data and checks, unit-tested.
//!
//! An identity belongs to a network (its name in the server list, such as `Libera.Chat`) or to one server (its
//! host name); a host's identity wins over its network's. What an identity leaves empty comes from the general
//! settings. **No password is ever stored**: the SASL account name is, the SASL password (and a server
//! password, when the identity says the server wants one) is asked for each time a connection is made.

pub const MAX_IDENTITIES: usize = 40;
/// Most alternate nicknames.
pub const MAX_ALTS: usize = 5;
/// Longest nickname we will send (servers allow less or a little more; they have the last word).
pub const MAX_NICK: usize = 30;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    /// A network name or a host name.
    pub network: String,
    pub nick: String,
    pub alt_nicks: Vec<String>,
    pub user: String,
    pub realname: String,
    pub sasl_account: String,
    /// Ask for a server password (PASS) every time, for a server that wants one.
    pub ask_password: bool,
}

impl Identity {
    /// Nothing is set: such an identity is not kept.
    pub fn is_blank(&self) -> bool {
        self.nick.is_empty()
            && self.alt_nicks.is_empty()
            && self.user.is_empty()
            && self.realname.is_empty()
            && self.sasl_account.is_empty()
            && !self.ask_password
    }
}

/// A nickname we are willing to send: not too long, no spaces, controls or IRC separators, and not starting with
/// a digit, a hyphen or a channel or mode character.
pub fn valid_nick(n: &str) -> bool {
    let first_ok = n.chars().next().is_some_and(|c| !c.is_ascii_digit() && !"-#&+%$:~@!".contains(c));
    first_ok && n.chars().count() <= MAX_NICK && !n.chars().any(|c| c.is_control() || c.is_whitespace() || ",!@*?:".contains(c))
}

/// A user name (the `ident` part of `nick!user@host`).
pub fn valid_user(u: &str) -> bool {
    !u.is_empty() && u.chars().count() <= 32 && !u.chars().any(|c| c.is_control() || c.is_whitespace() || "@!:".contains(c))
}

pub fn valid_realname(r: &str) -> bool {
    r.chars().count() <= 100 && !r.chars().any(|c| c.is_control())
}

/// A SASL account name: no spaces or controls (and no NUL, which separates the fields of SASL PLAIN).
pub fn valid_account(a: &str) -> bool {
    a.chars().count() <= 100 && !a.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// A network or host name: no spaces or control characters, and not absurdly long.
pub fn valid_network(n: &str) -> bool {
    !n.is_empty() && n.chars().count() <= 60 && !n.chars().any(|c| c.is_control() || c.is_whitespace() && c != ' ')
}

/// The alternate nicknames typed into the dialog: separated by commas or spaces, each valid, no repeats.
pub fn parse_alts(text: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split([',', ' ', ';']).map(str::trim).filter(|w| !w.is_empty()) {
        if !valid_nick(word) {
            return Err(format!("\"{word}\" is not a usable nickname."));
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(word)) {
            out.push(word.to_owned());
        }
    }
    if out.len() > MAX_ALTS {
        return Err(format!("At most {MAX_ALTS} alternate nicknames."));
    }
    Ok(out)
}

/// Check the fields of the dialog and make an identity of them.
pub fn build(network: &str, nick: &str, alts: &str, user: &str, realname: &str, sasl_account: &str, ask_password: bool) -> Result<Identity, String> {
    let network = network.trim();
    if !valid_network(network) {
        return Err("Enter a network name (as in the server list) or a server host name.".into());
    }
    let (nick, user, realname, sasl_account) = (nick.trim(), user.trim(), realname.trim(), sasl_account.trim());
    if !nick.is_empty() && !valid_nick(nick) {
        return Err("The nickname has spaces or characters a nickname cannot have.".into());
    }
    if !user.is_empty() && !valid_user(user) {
        return Err("The user name cannot have spaces, @, ! or :.".into());
    }
    if !valid_realname(realname) {
        return Err("The real name is too long (100 characters) or has control characters.".into());
    }
    if !valid_account(sasl_account) {
        return Err("The SASL account cannot have spaces.".into());
    }
    let alt_nicks = parse_alts(alts)?;
    Ok(Identity { network: network.to_owned(), nick: nick.to_owned(), alt_nicks, user: user.to_owned(), realname: realname.to_owned(), sasl_account: sasl_account.to_owned(), ask_password })
}

/// Replace the identity of its network; a blank one removes it.
pub fn set(list: &mut Vec<Identity>, id: Identity) -> Result<(), String> {
    list.retain(|i| !i.network.eq_ignore_ascii_case(&id.network));
    if !id.is_blank() {
        if list.len() >= MAX_IDENTITIES {
            return Err(format!("There can be at most {MAX_IDENTITIES} identities."));
        }
        list.push(id);
    }
    Ok(())
}

pub fn remove(list: &mut Vec<Identity>, network: &str) -> bool {
    let before = list.len();
    list.retain(|i| !i.network.eq_ignore_ascii_case(network));
    list.len() != before
}

/// The identity that applies to a connection to `host`: one named after the host first, then one named after
/// the network the host belongs to.
pub fn find<'a>(list: &'a [Identity], host: &str, network: Option<&str>) -> Option<&'a Identity> {
    list.iter()
        .find(|i| i.network.eq_ignore_ascii_case(host))
        .or_else(|| network.and_then(|n| list.iter().find(|i| i.network.eq_ignore_ascii_case(n))))
}

fn flat(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// One `settings.ini` value: TAB-separated `network nick alts user realname sasl-account ask-password`.
pub fn to_setting(i: &Identity) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}",
        flat(&i.network),
        flat(&i.nick),
        flat(&i.alt_nicks.join(",")),
        flat(&i.user),
        flat(&i.realname),
        flat(&i.sasl_account),
        if i.ask_password { 1 } else { 0 }
    )
}

pub fn from_setting(v: &str) -> Option<Identity> {
    let p: Vec<&str> = v.split('\t').collect();
    if p.len() != 7 {
        return None;
    }
    build(p[0], p[1], p[2], p[3], p[4], p[5], p[6].trim() == "1").ok().filter(|i| !i.is_blank())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(network: &str, nick: &str) -> Identity {
        build(network, nick, "", "", "", "", false).unwrap()
    }

    #[test]
    fn nicknames_users_and_accounts_are_checked() {
        for ok in ["alice", "A_b-c", "[x]", "w0w", "caf\u{e9}"] {
            assert!(valid_nick(ok), "{ok}");
        }
        for bad in ["", "1abc", "-a", "#chan", "a b", "a,b", "a!b", "a@b", "a:b", "a\u{7}b", &"x".repeat(31)] {
            assert!(!valid_nick(bad), "{bad:?}");
        }
        assert!(valid_user("me") && !valid_user("") && !valid_user("a b") && !valid_user("a@b") && !valid_user("a:b"));
        assert!(valid_account("alice") && valid_account("") && !valid_account("a b") && !valid_account("a\0b"));
        assert!(valid_realname("Alice A.") && valid_realname("") && !valid_realname("a\nb"));
    }

    #[test]
    fn alternates_split_dedupe_and_are_capped() {
        assert_eq!(parse_alts("a1, a2 a3;A1"), Ok(vec!["a1".into(), "a2".into(), "a3".into()]));
        assert_eq!(parse_alts(""), Ok(vec![]));
        assert!(parse_alts("good, 1bad").is_err());
        assert!(parse_alts("a1 a2 a3 a4 a5 a6").is_err());
    }

    #[test]
    fn building_rejects_bad_fields_with_a_reason() {
        assert!(build("", "x", "", "", "", "", false).is_err());
        assert!(build("net", "bad nick", "", "", "", "", false).is_err());
        assert!(build("net", "", "", "a@b", "", "", false).is_err());
        assert!(build("net", "", "", "", "a\tb", "", false).is_err());
        assert!(build("net", "", "", "", "", "my account", false).is_err());
        assert!(build("net", "", "ok 1bad", "", "", "", false).is_err());
        let ok = build(" Libera.Chat ", " alice ", "alice2,alice3", "al", "Alice A.", "alice", true).unwrap();
        assert_eq!((ok.network.as_str(), ok.nick.as_str(), ok.alt_nicks.len(), ok.ask_password), ("Libera.Chat", "alice", 2, true));
    }

    #[test]
    fn a_host_identity_wins_over_its_network_and_blank_ones_are_dropped() {
        let mut list = Vec::new();
        set(&mut list, id("Libera.Chat", "net_nick")).unwrap();
        set(&mut list, id("irc.libera.chat", "host_nick")).unwrap();
        assert_eq!(find(&list, "irc.libera.chat", Some("Libera.Chat")).unwrap().nick, "host_nick");
        assert_eq!(find(&list, "other.example", Some("LIBERA.CHAT")).unwrap().nick, "net_nick");
        assert!(find(&list, "other.example", None).is_none());
        // setting a network again replaces it; a blank one removes it
        set(&mut list, id("libera.chat", "newer")).unwrap();
        assert_eq!(list.len(), 2);
        set(&mut list, build("irc.libera.chat", "", "", "", "", "", false).unwrap()).unwrap();
        assert_eq!(list.len(), 1);
        assert!(remove(&mut list, "LIBERA.CHAT") && list.is_empty() && !remove(&mut list, "x"));
    }

    #[test]
    fn the_list_is_capped() {
        let mut list = Vec::new();
        for n in 0..MAX_IDENTITIES {
            set(&mut list, id(&format!("net{n}"), "x")).unwrap();
        }
        assert!(set(&mut list, id("one-too-many", "x")).is_err());
        assert!(set(&mut list, id("net0", "changed")).is_ok(), "replacing is still fine");
    }

    #[test]
    fn settings_roundtrip_and_hostile_lines_are_ignored() {
        let i = build("Libera.Chat", "alice", "a2, a3", "al", "Alice A.", "alice", true).unwrap();
        assert_eq!(from_setting(&to_setting(&i)), Some(i.clone()));
        assert_eq!(to_setting(&i).matches('\t').count(), 6);
        for junk in ["", "one", "a\tb", "net\t\t\t\t\t\t0", "net\tbad nick\t\t\t\t\t0", "net\t1abc\t\t\t\t\t0", "net\tx\t\t\t\t\t0\textra", "\tx\t\t\t\t\t0"] {
            assert!(from_setting(junk).is_none(), "{junk:?}");
        }
        // a settings line carries no password field at all
        assert!(!to_setting(&i).to_lowercase().contains("password"));
    }
}
