//! IRC host masks: `nick!user@host` patterns with `*` and `?` wildcards, matched
//! case-insensitively using RFC 1459 case folding (`[]\~` equal `{}|^`).

use crate::Source;

fn fold(c: char) -> char {
    match c {
        'A'..='Z' => c.to_ascii_lowercase(),
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        '~' => '^',
        c => c.to_lowercase().next().unwrap_or(c),
    }
}

/// Does `text` match `pattern`? `*` matches any run (including empty), `?` exactly one character.
/// Linear-ish (no exponential backtracking), so hostile patterns cannot hang the client.
pub fn wildmatch(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().map(fold).collect();
    let t: Vec<char> = text.chars().map(fold).collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    // Where the last `*` was, and which text position it is currently absorbing up to.
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) && p[pi] != '*' {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Turn what a user typed into a full `nick!user@host` mask:
/// `troll` -> `troll!*@*`, `*@bad.host` -> `*!*@bad.host`, `troll!bob` -> `troll!bob@*`.
pub fn normalize(input: &str) -> String {
    let s = input.trim();
    match (s.contains('!'), s.contains('@')) {
        (true, true) => s.to_owned(),
        (true, false) => format!("{s}@*"),
        (false, true) => format!("*!{s}"),
        (false, false) => format!("{s}!*@*"),
    }
}

impl Source {
    /// `nick!user@host` for a user source; `None` for servers (no user or host part), which
    /// masks never apply to.
    pub fn mask(&self) -> Option<String> {
        match (&self.user, &self.host) {
            (Some(u), Some(h)) => Some(format!("{}!{u}@{h}", self.name)),
            _ => None,
        }
    }
}

/// Does `mask` (already normalised) match this message source?
pub fn matches_source(mask: &str, src: &Source) -> bool {
    src.mask().is_some_and(|m| wildmatch(mask, &m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildmatch("*", ""));
        assert!(wildmatch("*", "anything"));
        assert!(wildmatch("a*c", "abc") && wildmatch("a*c", "ac") && wildmatch("a*c", "abbbc"));
        assert!(!wildmatch("a*c", "abd") && !wildmatch("a*c", "acb"));
        assert!(wildmatch("a?c", "abc") && !wildmatch("a?c", "ac") && !wildmatch("a?c", "abbc"));
        assert!(wildmatch("*!*@*.example.net", "nick!user@host.example.net"));
        assert!(!wildmatch("*!*@*.example.net", "nick!user@example.org"));
        assert!(wildmatch("**a**", "xax"));
        assert!(!wildmatch("", "x") && wildmatch("", ""));
    }

    #[test]
    fn case_folding_follows_rfc1459() {
        assert!(wildmatch("NICK!*@*", "nick!u@h"));
        assert!(wildmatch("[away]!*@*", "{AWAY}!u@h")); // [ ] equal { }
        assert!(wildmatch("a\\b!*@*", "a|b!u@h"));
        assert!(wildmatch("~x!*@*", "^X!u@h"));
        assert!(wildmatch("|-|ydra!*@*", "\\-\\YDRA!u@h"));
    }

    #[test]
    fn hostile_patterns_do_not_blow_up() {
        let text = "a".repeat(2000);
        assert!(!wildmatch("a*a*a*a*a*a*a*a*a*a*b", &text));
        assert!(wildmatch("a*a*a*a*a*a*a*a*a*a*a", &text));
    }

    #[test]
    fn normalising_what_users_type() {
        assert_eq!(normalize("troll"), "troll!*@*");
        assert_eq!(normalize("*@bad.host"), "*!*@bad.host");
        assert_eq!(normalize("troll!bob"), "troll!bob@*");
        assert_eq!(normalize("a!b@c"), "a!b@c");
        assert_eq!(normalize("  spaced  "), "spaced!*@*");
    }

    #[test]
    fn matching_sources_never_matches_servers() {
        let user = Source::parse("Troll!bob@evil.host");
        assert!(matches_source("troll!*@*", &user));
        assert!(matches_source("*!*@evil.host", &user));
        assert!(!matches_source("*!*@good.host", &user));
        let server = Source::parse("irc.example.net");
        assert!(!matches_source("*!*@*", &server), "a catch-all mask must not silence the server");
        let partial = Source::parse("nick");
        assert!(!matches_source("*", &partial));
    }
}
