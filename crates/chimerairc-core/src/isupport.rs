//! `RPL_ISUPPORT` (005) tracking.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseMapping {
    Ascii,
    Rfc1459,
    Rfc1459Strict,
    /// `rfc7613`: Unicode-aware; approximated with Unicode lowercase.
    Rfc7613,
}

impl CaseMapping {
    /// Fold a nick or channel name for comparison.
    pub fn fold(self, s: &str) -> String {
        match self {
            CaseMapping::Rfc7613 => s.to_lowercase(),
            _ => s
                .chars()
                .map(|c| match c {
                    'A'..='Z' => c.to_ascii_lowercase(),
                    '[' if self != CaseMapping::Ascii => '{',
                    ']' if self != CaseMapping::Ascii => '}',
                    '\\' if self != CaseMapping::Ascii => '|',
                    '~' if self == CaseMapping::Rfc1459 => '^',
                    c => c,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ISupport {
    pub network: Option<String>,
    /// (mode letter, prefix symbol) from highest to lowest rank, e.g. `[('o','@'),('v','+')]`.
    pub prefix: Vec<(char, char)>,
    pub chantypes: String,
    /// CHANMODES groups A (list), B (always arg), C (arg when set), D (no arg).
    pub chanmodes: [String; 4],
    pub casemapping: CaseMapping,
    pub utf8only: bool,
    pub raw: HashMap<String, String>,
}

impl Default for ISupport {
    fn default() -> Self {
        ISupport {
            network: None,
            prefix: vec![('o', '@'), ('v', '+')],
            chantypes: "#&".into(),
            chanmodes: ["b".into(), "k".into(), "l".into(), "imnpst".into()],
            casemapping: CaseMapping::Rfc1459,
            utf8only: false,
            raw: HashMap::new(),
        }
    }
}

fn unescape(v: &str) -> String {
    v.replace("\\x20", " ").replace("\\x3D", "=").replace("\\x5C", "\\")
}

impl ISupport {
    /// Apply the tokens of one 005 line (everything between the nick and the
    /// trailing "are supported by this server" text).
    pub fn apply(&mut self, tokens: &[String]) {
        for t in tokens {
            if let Some(key) = t.strip_prefix('-') {
                self.raw.remove(key);
                if key == "NETWORK" {
                    self.network = None;
                }
                continue;
            }
            let (key, val) = t.split_once('=').map_or((t.as_str(), ""), |(k, v)| (k, v));
            let val = unescape(val);
            match key {
                "NETWORK" => self.network = Some(val.clone()),
                "CHANTYPES" => self.chantypes = val.clone(),
                "UTF8ONLY" => self.utf8only = true,
                "CASEMAPPING" => {
                    self.casemapping = match val.as_str() {
                        "ascii" => CaseMapping::Ascii,
                        "rfc1459-strict" => CaseMapping::Rfc1459Strict,
                        "rfc7613" => CaseMapping::Rfc7613,
                        _ => CaseMapping::Rfc1459,
                    }
                }
                "PREFIX" => {
                    // (ov)@+
                    if let Some(rest) = val.strip_prefix('(') {
                        if let Some((modes, syms)) = rest.split_once(')') {
                            self.prefix = modes.chars().zip(syms.chars()).collect();
                        }
                    }
                }
                "CHANMODES" => {
                    let mut it = val.split(',');
                    for slot in self.chanmodes.iter_mut() {
                        *slot = it.next().unwrap_or("").to_owned();
                    }
                }
                _ => {}
            }
            self.raw.insert(key.to_owned(), val);
        }
    }

    pub fn is_channel(&self, target: &str) -> bool {
        target.chars().next().is_some_and(|c| self.chantypes.contains(c))
    }

    pub fn prefix_mode_for_symbol(&self, sym: char) -> Option<char> {
        self.prefix.iter().find(|(_, s)| *s == sym).map(|(m, _)| *m)
    }
}

/// Whether the client tag `tag` (without the leading `+`) may be sent, given the `CLIENTTAGDENY` value of the
/// server (`None` when it did not send one). The value is a comma list: `*` denies every tag, a plain name
/// denies that tag, and `-name` takes a tag out of a `*` denial.
pub fn client_tag_allowed(deny: Option<&str>, tag: &str) -> bool {
    let Some(deny) = deny else { return true };
    let mut denied = false;
    for item in deny.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if item == "*" {
            denied = true;
        } else if item.strip_prefix('-') == Some(tag) {
            return true;
        } else if item == tag {
            denied = true;
        }
    }
    !denied
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clienttagdeny_rules() {
        assert!(client_tag_allowed(None, "typing"));
        assert!(client_tag_allowed(Some(""), "typing"));
        assert!(!client_tag_allowed(Some("*"), "typing"));
        assert!(client_tag_allowed(Some("*,-typing"), "typing"));
        assert!(!client_tag_allowed(Some("*,-draft/reply"), "typing"));
        assert!(!client_tag_allowed(Some("typing,draft/react"), "typing"));
        assert!(client_tag_allowed(Some("draft/react"), "typing"));
        assert!(client_tag_allowed(Some("-typing,*"), "typing"));
    }

    fn toks(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn parses_common_tokens() {
        let mut i = ISupport::default();
        i.apply(&toks("NETWORK=Libera\\x20Chat PREFIX=(qaohv)~&@%+ CHANMODES=beI,k,l,imnpst CASEMAPPING=ascii CHANTYPES=# UTF8ONLY"));
        assert_eq!(i.network.as_deref(), Some("Libera Chat"));
        assert_eq!(i.prefix[0], ('q', '~'));
        assert_eq!(i.prefix.len(), 5);
        assert_eq!(i.chanmodes[0], "beI");
        assert_eq!(i.casemapping, CaseMapping::Ascii);
        assert!(i.utf8only);
        assert!(i.is_channel("#x") && !i.is_channel("&x"));
        assert_eq!(i.prefix_mode_for_symbol('@'), Some('o'));
        i.apply(&toks("-UTF8ONLY"));
        assert!(!i.raw.contains_key("UTF8ONLY"));
    }

    #[test]
    fn folding() {
        assert_eq!(CaseMapping::Rfc1459.fold("Nick[A]~"), "nick{a}^");
        assert_eq!(CaseMapping::Rfc1459Strict.fold("N[~"), "n{~");
        assert_eq!(CaseMapping::Ascii.fold("N[A]"), "n[a]");
    }
}
