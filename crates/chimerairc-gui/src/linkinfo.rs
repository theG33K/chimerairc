//! What a click or a hover in the chat text means: the word under the pointer, whether it is a channel or a
//! nickname, and a safe description of a web address (shown while the pointer is over it). Pure text, unit-tested.
//!
//! Nothing here touches the network: the description is made from the address itself. (Fetching a page to show
//! its title would tell that site your address, so it is deliberately not done.)

/// Characters that end a word in chat: spaces and the brackets and punctuation people put around names.
fn is_separator(c: char) -> bool {
    c.is_whitespace() || c.is_control() || "<>()[]{}\"'`,;!?|\u{201c}\u{201d}".contains(c)
}

/// The word around position `pos` (a character index into `text`), without trailing punctuation. `None` when
/// `pos` is on a separator or past the end.
pub fn word_at(text: &str, pos: usize) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    if pos >= chars.len() || is_separator(chars[pos]) {
        return None;
    }
    let mut a = pos;
    while a > 0 && !is_separator(chars[a - 1]) {
        a -= 1;
    }
    let mut b = pos + 1;
    while b < chars.len() && !is_separator(chars[b]) {
        b += 1;
    }
    let mut word: String = chars[a..b].iter().collect();
    while word.ends_with(['.', ':', '-']) && word.chars().count() > 1 {
        word.pop();
    }
    (!word.is_empty()).then_some(word)
}

/// What a word names.
#[derive(Debug, PartialEq, Eq)]
pub enum Named {
    Channel(String),
    Nick(String),
}

/// A channel name (`#rust`, `&local`) or a nickname, with a rank symbol in front of a nick (`@alice`) removed.
pub fn classify(word: &str) -> Option<Named> {
    if word.starts_with(['#', '&']) {
        return crate::favorites::valid_channel(word).then(|| Named::Channel(word.to_owned()));
    }
    let nick = word.trim_start_matches(['@', '+', '%', '~', '&']);
    crate::identity::valid_nick(nick).then(|| Named::Nick(nick.to_owned()))
}

/// How much of a long address is shown.
const SHOWN: usize = 110;

/// `Link: example.org - https://example.org/path`, plus a warning when the address could mislead: a name in front
/// of an `@` (the real site is what follows it), an internationalized (`xn--`) name, or a raw IP number.
/// `None` for anything but http and https.
pub fn describe(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    let host = if hostport.starts_with('[') {
        hostport.split(']').next().map(|h| format!("{h}]")).unwrap_or_default()
    } else {
        hostport.split(':').next().unwrap_or("").to_owned()
    };
    if host.is_empty() {
        return None;
    }
    let mut notes: Vec<String> = Vec::new();
    if userinfo.is_some() {
        notes.push(format!("the text before the @ is not the site: this goes to {host}"));
    }
    if host.split('.').any(|l| l.starts_with("xn--")) {
        notes.push("an internationalized name, check it is the site you expect".to_owned());
    }
    let digits = host.split('.').count() == 4 && host.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    if digits || host.starts_with('[') {
        notes.push("a raw IP number, not a name".to_owned());
    }
    if !host.is_ascii() {
        notes.push("the name has unusual characters".to_owned());
    }
    let mut shown: String = url.chars().take(SHOWN).collect();
    if url.chars().count() > SHOWN {
        shown.push_str("...");
    }
    let warn = if notes.is_empty() { String::new() } else { format!("   (Careful: {})", notes.join("; ")) };
    Some(format!("Link: {host} - {shown}{warn}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_word_under_the_pointer_is_found_without_punctuation() {
        let t = "<alice> see #rust, or ask @bob. Thanks (carol)!";
        assert_eq!(word_at(t, 3).as_deref(), Some("alice"));
        assert_eq!(word_at(t, 13).as_deref(), Some("#rust"));
        assert_eq!(word_at(t, 14).as_deref(), Some("#rust"));
        assert_eq!(word_at(t, 26).as_deref(), Some("@bob"), "the trailing dot is not part of it");
        assert_eq!(word_at(t, 40).as_deref(), Some("carol"));
        assert_eq!(word_at(t, 7), None, "on a separator");
        assert_eq!(word_at(t, 500), None, "past the end");
        assert_eq!(word_at("", 0), None);
        assert_eq!(word_at("...", 1).as_deref(), Some("."), "a lone mark stays a word of its own");
    }

    #[test]
    fn words_are_classified_as_channels_or_nicks() {
        assert_eq!(classify("#rust"), Some(Named::Channel("#rust".into())));
        assert_eq!(classify("&local"), Some(Named::Channel("&local".into())));
        assert_eq!(classify("@alice"), Some(Named::Nick("alice".into())));
        assert_eq!(classify("+bob"), Some(Named::Nick("bob".into())));
        assert_eq!(classify("carol"), Some(Named::Nick("carol".into())));
        for none in ["#", "#a,b", "12345", "", "@", "-x", "a:b"] {
            assert_eq!(classify(none), None, "{none:?}");
        }
    }

    #[test]
    fn a_plain_address_is_shown_with_its_site() {
        assert_eq!(describe("https://example.org/a/b?x=1").as_deref(), Some("Link: example.org - https://example.org/a/b?x=1"));
        assert_eq!(describe("HTTP://Example.ORG:8080/x").as_deref(), Some("Link: example.org - HTTP://Example.ORG:8080/x"));
        assert_eq!(describe("ftp://example.org/x"), None);
        assert_eq!(describe("https://"), None);
        assert_eq!(describe("javascript:alert(1)"), None);
    }

    #[test]
    fn misleading_addresses_carry_a_warning() {
        let s = describe("https://paypal.com@evil.example/login").unwrap();
        assert!(s.starts_with("Link: evil.example - ") && s.contains("not the site: this goes to evil.example"), "{s}");
        let s = describe("https://xn--pple-43d.com/").unwrap();
        assert!(s.contains("internationalized"), "{s}");
        let s = describe("http://192.168.0.1/admin").unwrap();
        assert!(s.contains("raw IP number"), "{s}");
        let s = describe("http://[::1]:80/").unwrap();
        assert!(s.contains("raw IP number") && s.contains("[::1]"), "{s}");
        assert!(!describe("https://example.org/").unwrap().contains("Careful"));
    }

    #[test]
    fn long_addresses_are_cut() {
        let u = format!("https://example.org/{}", "a".repeat(300));
        let s = describe(&u).unwrap();
        assert!(s.ends_with("...") && s.chars().count() < 160, "{}", s.chars().count());
    }
}
