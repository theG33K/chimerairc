//! Wording for what the IRCv3 capabilities tell us (extended-join, netsplit/netjoin batches, away marks).
//! Pure text, so it is tested without a window.

/// The text added after a nick in a join line when `extended-join` gave us the account and real name:
/// ` (account: bob, real name: Bob B.)`, or nothing when the server gave neither.
pub fn join_details(account: Option<&str>, realname: Option<&str>) -> String {
    let account = account.filter(|a| !a.is_empty() && *a != "*");
    let realname = realname.filter(|r| !r.is_empty());
    match (account, realname) {
        (None, None) => String::new(),
        (Some(a), None) => format!(" (account: {a})"),
        (None, Some(r)) => format!(" (real name: {r})"),
        (Some(a), Some(r)) => format!(" (account: {a}, real name: {r})"),
    }
}

/// Most names written out in a netsplit line; the rest are counted.
const SHOWN: usize = 8;

/// One line for a whole netsplit (`split`) or netjoin: `* Netsplit a.example <-> b.example: 3 users quit (x, y, z)`.
pub fn split_line(split: bool, servers: &[String], nicks: &[String]) -> String {
    let between = match servers {
        [a, b, ..] => format!(" {a} <-> {b}"),
        [a] => format!(" {a}"),
        [] => String::new(),
    };
    let n = nicks.len();
    let people = format!("{n} user{}", if n == 1 { "" } else { "s" });
    let shown: Vec<&str> = nicks.iter().take(SHOWN).map(String::as_str).collect();
    let more = if n > SHOWN { format!(", and {} more", n - SHOWN) } else { String::new() };
    let names = if shown.is_empty() { String::new() } else { format!(" ({}{more})", shown.join(", ")) };
    if split {
        format!("* Netsplit{between}: {people} quit{names}")
    } else {
        format!("* Netjoin{between}: {people} back{names}")
    }
}

/// The nick list text for one member: the rank symbol, the nick, and `(away)` for someone marked away.
pub fn member_text(sym: &str, nick: &str, away: bool) -> String {
    if away {
        format!("{sym}{nick} (away)")
    } else {
        format!("{sym}{nick}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn join_details_say_only_what_is_known() {
        assert_eq!(join_details(None, None), "");
        assert_eq!(join_details(Some("*"), Some("")), "");
        assert_eq!(join_details(Some("bob"), None), " (account: bob)");
        assert_eq!(join_details(None, Some("Bob B.")), " (real name: Bob B.)");
        assert_eq!(join_details(Some("bob"), Some("Bob B.")), " (account: bob, real name: Bob B.)");
    }

    #[test]
    fn a_split_names_the_servers_and_the_first_few_people() {
        let s = split_line(true, &v(&["a.example", "b.example"]), &v(&["x", "y", "z"]));
        assert_eq!(s, "* Netsplit a.example <-> b.example: 3 users quit (x, y, z)");
        assert_eq!(split_line(false, &v(&["a.example", "b.example"]), &v(&["x"])), "* Netjoin a.example <-> b.example: 1 user back (x)");
    }

    #[test]
    fn a_big_split_counts_the_rest_and_missing_servers_are_fine() {
        let many: Vec<String> = (1..=12).map(|i| format!("n{i}")).collect();
        let s = split_line(true, &[], &many);
        assert!(s.starts_with("* Netsplit: 12 users quit (n1, n2, n3, n4, n5, n6, n7, n8, and 4 more)"), "{s}");
        assert_eq!(split_line(true, &v(&["only.example"]), &[]), "* Netsplit only.example: 0 users quit");
    }

    #[test]
    fn away_members_are_marked_in_the_list() {
        assert_eq!(member_text("@", "op", false), "@op");
        assert_eq!(member_text("", "bob", true), "bob (away)");
    }
}
