//! Finding web addresses in chat for the URL Catcher pane. Pure text handling, unit-tested.
//!
//! Only `http://` and `https://` addresses are ever collected (the same rule as clicking a link in a
//! window), so nothing like `file:`, `javascript:` or `ms-msdt:` can end up in the list.

use chimerairc_proto::format;

/// Longest address kept; anything longer is more likely junk than a link.
const MAX_URL: usize = 2000;

fn is_boundary(c: char) -> bool {
    c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '"' | '`' | '\\' | '^' | '{' | '|')
}

/// Strip punctuation that belongs to the sentence rather than the address: `see https://x.org/a.`
/// and `(https://x.org/a)` both mean `https://x.org/a`, while `https://en.wikipedia.org/wiki/A_(B)`
/// keeps its closing bracket because it is balanced.
fn trim_tail(mut s: &str) -> &str {
    loop {
        let Some(last) = s.chars().last() else { return s };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' | '~' => true,
            ')' => s.matches(')').count() > s.matches('(').count(),
            ']' => s.matches(']').count() > s.matches('[').count(),
            '}' => s.matches('}').count() > s.matches('{').count(),
            _ => false,
        };
        if !drop {
            return s;
        }
        s = &s[..s.len() - last.len_utf8()];
    }
}

/// Every http(s) address in `text` (IRC formatting codes are ignored), in order, without repeats.
pub fn extract(text: &str) -> Vec<String> {
    let plain = format::strip(text);
    let lower = plain.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut from = 0;
    while from < plain.len() {
        let rest = &lower[from..];
        let hit = match (rest.find("http://"), rest.find("https://")) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        let start = from + hit;
        // must start a word: `xhttp://` is not a link
        let starts_word = plain[..start].chars().last().map_or(true, |c| !c.is_alphanumeric());
        let end = plain[start..].find(is_boundary).map_or(plain.len(), |i| start + i);
        let candidate = trim_tail(&plain[start..end]);
        let scheme_len = if lower[start..].starts_with("https://") { 8 } else { 7 };
        let after = candidate.get(scheme_len..).unwrap_or("");
        // a host must start like one (not another scheme: `http://http://`)
        let host_ok = after.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '[')
            && !after.to_ascii_lowercase().starts_with("http://")
            && !after.to_ascii_lowercase().starts_with("https://");
        if starts_word && host_ok && candidate.len() <= MAX_URL && !out.iter().any(|u| u == candidate) {
            out.push(candidate.to_owned());
        }
        from = end.max(start + 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_plain_addresses() {
        assert_eq!(extract("see https://example.org/page?x=1&y=2 for details"), ["https://example.org/page?x=1&y=2"]);
        assert_eq!(extract("http://a.example and HTTPS://B.EXAMPLE/x"), ["http://a.example", "HTTPS://B.EXAMPLE/x"]);
        assert_eq!(extract("no links here, just text with http and https words"), Vec::<String>::new());
        assert_eq!(extract("first https://a.example/1 then https://a.example/1 again"), ["https://a.example/1"], "no repeats");
    }

    #[test]
    fn sentence_punctuation_is_not_part_of_the_address() {
        for (text, want) in [
            ("go to https://x.org/a.", "https://x.org/a"),
            ("(https://x.org/a)", "https://x.org/a"),
            ("<https://x.org/a>", "https://x.org/a"),
            ("\"https://x.org/a\", right?", "https://x.org/a"),
            ("really? https://x.org/a?!", "https://x.org/a"),
            ("[https://x.org/a]", "https://x.org/a"),
            ("https://en.wikipedia.org/wiki/Rust_(programming_language)", "https://en.wikipedia.org/wiki/Rust_(programming_language)"),
            ("(see https://en.wikipedia.org/wiki/Rust_(programming_language))", "https://en.wikipedia.org/wiki/Rust_(programming_language)"),
            ("https://x.org/a;", "https://x.org/a"),
        ] {
            assert_eq!(extract(text), [want], "{text}");
        }
    }

    #[test]
    fn irc_formatting_codes_do_not_break_or_sneak_into_addresses() {
        assert_eq!(extract("\x0304https://x.org/red\x03 text"), ["https://x.org/red"]);
        assert_eq!(extract("\x02bold \x02https://x.org/b\x0f!"), ["https://x.org/b"]);
        // formatting codes are invisible, so the address is what is shown
        assert_eq!(extract("https://x.org/a\x02b"), ["https://x.org/ab"]);
    }

    #[test]
    fn only_web_addresses_with_a_host_are_collected() {
        for bad in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "ms-msdt:/id PCWDiagnostic",
            "ftp://x.org/f",
            "xhttp://x.org",
            "http://",
            "https://.",
            "http:// space",
            "https:///path-only",
        ] {
            assert!(extract(bad).is_empty(), "{bad:?} must not be collected");
        }
        assert_eq!(extract("http://[::1]:8080/x"), ["http://[::1]:8080/x"], "IPv6 literals are fine");
        assert_eq!(extract("http://127.0.0.1:6667/"), ["http://127.0.0.1:6667/"]);
    }

    #[test]
    fn absurdly_long_addresses_are_ignored() {
        let long = format!("https://x.org/{}", "a".repeat(MAX_URL));
        assert!(extract(&long).is_empty());
        let ok = format!("https://x.org/{}", "a".repeat(1000));
        assert_eq!(extract(&ok).len(), 1);
    }

    #[test]
    fn several_addresses_and_odd_input_do_not_panic() {
        let text = "a https://one.example/ b http://two.example/x, c (https://three.example/y).";
        assert_eq!(extract(text), ["https://one.example/", "http://two.example/x", "https://three.example/y"]);
        assert!(extract("").is_empty());
        assert_eq!(extract("https://é.example/ü"), ["https://é.example/ü"]);
        assert!(extract("http://http://").is_empty());
        for junk in ["\u{0}\u{1}http", "https", "http:/", "ttps://x.org", "\x03\x03\x03https://"] {
            let _ = extract(junk);
        }
    }
}
