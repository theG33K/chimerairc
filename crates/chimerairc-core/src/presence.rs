//! Helpers for tracking whether chosen nicknames are online (IRCv3 `MONITOR`, or `ISON` polling on
//! servers without it). The state lives in [`crate::Session`]; this module holds the pure parts.

/// Longest nickname we will watch.
pub const MAX_NICK: usize = 50;
/// Most nicknames one watch list may hold.
pub const MAX_WATCH: usize = 500;
/// A command's list is split so that the line stays well inside the 512-byte limit.
pub const CHUNK_BYTES: usize = 300;

/// Is `s` something we may put in a MONITOR/ISON list? (No separators, not a channel, not empty.)
pub fn valid_nick(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_NICK
        && !s.starts_with(['#', '&', '+', '!', ':', '-', '@', '$', '*', '?'])
        && !s.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, ',' | '!' | '*' | '?' | '@' | '.'))
}

/// Split `nicks` into groups whose joined length (with one separator byte between names) stays within
/// `max` bytes. A single name longer than `max` still gets a group of its own.
pub fn chunks(nicks: &[String], max: usize) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut len = 0;
    for n in nicks {
        match out.last_mut() {
            Some(g) if len + 1 + n.len() <= max => {
                g.push(n.clone());
                len += 1 + n.len();
            }
            _ => {
                out.push(vec![n.clone()]);
                len = n.len();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nicknames_that_could_break_a_list_are_refused() {
        for ok in ["bob", "Bob_2", "[away]", "a\\b", "x|y", "`tick"] {
            assert!(valid_nick(ok), "{ok}");
        }
        for bad in ["", "#chan", "a b", "a,b", "a\nb", "nick!user@host", "*", "a*b", "-x", ":x", "a.b", &"n".repeat(51)] {
            assert!(!valid_nick(bad), "{bad:?}");
        }
    }

    #[test]
    fn lists_are_split_to_fit_a_line() {
        let v: Vec<String> = (0..10).map(|i| format!("nick{i}")).collect();
        let c = chunks(&v, 20);
        assert!(c.len() > 1);
        assert_eq!(c.concat(), v, "nothing lost, order kept");
        assert!(c.iter().all(|g| g.join(" ").len() <= 20));
        assert!(chunks(&[], 20).is_empty());
        let long = vec!["x".repeat(40)];
        assert_eq!(chunks(&long, 20), vec![long]);
    }
}
