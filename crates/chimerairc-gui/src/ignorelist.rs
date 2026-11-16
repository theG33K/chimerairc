//! The user's ignore list: entries (a core [`IgnoreRule`] plus an on/off switch and notes), their
//! settings-file format and helpers. Pure logic, no Win32.
//!
//! Settings format, one line per entry (tab separated, because nicks may contain `|`):
//! `flags<TAB>network<TAB>channels<TAB>mask<TAB>notes` where flags is a mix of `m` (messages),
//! `n` (notices), `c` (CTCP) and `d` (disabled), or `-` for "all kinds, enabled".

use chimerairc_core::IgnoreRule;
use chimerairc_proto::mask;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub enabled: bool,
    pub notes: String,
    pub rule: IgnoreRule,
}

fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_owned()
}

impl Entry {
    /// An enabled, ignore-everything entry for a nick or mask.
    pub fn new(mask_or_nick: &str) -> Entry {
        Entry { enabled: true, notes: String::new(), rule: IgnoreRule::new(&clean(mask_or_nick)) }
    }

    pub fn to_setting(&self) -> String {
        let r = &self.rule;
        let mut flags = String::new();
        if r.messages {
            flags.push('m');
        }
        if r.notices {
            flags.push('n');
        }
        if r.ctcp {
            flags.push('c');
        }
        if !self.enabled {
            flags.push('d');
        }
        if flags.is_empty() {
            flags.push('-');
        }
        format!(
            "{flags}\t{}\t{}\t{}\t{}",
            clean(r.network.as_deref().unwrap_or("")),
            r.channels.iter().map(|c| clean(c)).collect::<Vec<_>>().join(","),
            clean(&r.mask),
            clean(&self.notes)
        )
    }

    pub fn from_setting(s: &str) -> Option<Entry> {
        let mut it = s.splitn(5, '\t');
        let (flags, network, channels, mask_text, notes) =
            (it.next()?, it.next()?, it.next()?, it.next()?, it.next().unwrap_or(""));
        let mask_text = mask_text.trim();
        if mask_text.is_empty() {
            return None;
        }
        let (m, n, c) = (flags.contains('m'), flags.contains('n'), flags.contains('c'));
        let all = !(m || n || c); // "-" (or junk) means every kind
        let network = Some(network.trim().to_owned()).filter(|n| !n.is_empty());
        let channels: Vec<String> =
            channels.split(',').map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()).collect();
        Some(Entry {
            enabled: !flags.contains('d'),
            notes: clean(notes),
            rule: IgnoreRule {
                mask: mask::normalize(mask_text),
                network,
                channels,
                messages: all || m,
                notices: all || n,
                ctcp: all || c,
            },
        })
    }

    /// One line for the list box and for `/ignore`.
    pub fn describe(&self) -> String {
        let r = &self.rule;
        let kinds: Vec<&str> = [(r.messages, "messages"), (r.notices, "notices"), (r.ctcp, "CTCP")]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, n)| *n)
            .collect();
        let mut s = format!("{} {}   ({})", if self.enabled { "[x]" } else { "[ ]" }, r.mask, kinds.join(", "));
        if !r.channels.is_empty() {
            s.push_str(&format!(" in {}", r.channels.join(", ")));
        }
        if let Some(n) = &r.network {
            s.push_str(&format!(" on {n}"));
        }
        if !self.notes.is_empty() {
            s.push_str(&format!(" \u{2014} {}", self.notes));
        }
        s
    }

    /// Two entries cover the same people in the same places.
    fn same_scope(&self, other: &Entry) -> bool {
        let lc = |v: &Vec<String>| {
            let mut x: Vec<String> = v.iter().map(|c| c.to_lowercase()).collect();
            x.sort();
            x
        };
        self.rule.mask.eq_ignore_ascii_case(&other.rule.mask)
            && self.rule.network.as_deref().map(str::to_lowercase) == other.rule.network.as_deref().map(str::to_lowercase)
            && lc(&self.rule.channels) == lc(&other.rule.channels)
    }
}

/// Add unless an entry with the same mask, network and channels exists. True if added.
pub fn add(list: &mut Vec<Entry>, e: Entry) -> bool {
    if list.iter().any(|x| x.same_scope(&e)) {
        return false;
    }
    list.push(e);
    true
}

/// Remove every entry whose mask is the normalised form of `mask_or_nick`. Returns how many.
pub fn remove(list: &mut Vec<Entry>, mask_or_nick: &str) -> usize {
    let target = mask::normalize(&clean(mask_or_nick));
    let before = list.len();
    list.retain(|e| !e.rule.mask.eq_ignore_ascii_case(&target));
    before - list.len()
}

/// Index of the plain "ignore this nick everywhere" entry for `nick`, if there is one.
pub fn find_nick(list: &[Entry], nick: &str) -> Option<usize> {
    let target = mask::normalize(nick);
    list.iter().position(|e| e.rule.mask.eq_ignore_ascii_case(&target))
}

/// The rules to hand to the connections: enabled entries only.
pub fn rules(list: &[Entry]) -> Vec<IgnoreRule> {
    list.iter().filter(|e| e.enabled).map(|e| e.rule.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_including_pipes_in_nicks_and_all_options() {
        let mut e = Entry::new("|-|ydra");
        e.notes = "old friend | not really".into();
        e.rule.network = Some("irc.example.net".into());
        e.rule.channels = vec!["#a".into(), "#b".into()];
        e.rule.notices = false;
        e.enabled = false;
        let line = e.to_setting();
        assert!(!line.contains('\n'));
        assert_eq!(Entry::from_setting(&line), Some(e));

        let plain = Entry::new("troll");
        assert_eq!(plain.to_setting(), "mnc\t\t\ttroll!*@*\t");
        // the settings reader trims the line, which drops the empty trailing field: still parses
        assert_eq!(Entry::from_setting(plain.to_setting().trim()), Some(plain));
    }

    #[test]
    fn tolerates_junk_and_defaults_to_everything() {
        assert_eq!(Entry::from_setting(""), None);
        assert_eq!(Entry::from_setting("mnc\t\t\t\tnotes"), None, "empty mask");
        assert_eq!(Entry::from_setting("garbage"), None);
        let e = Entry::from_setting("-\t\t\tbad").unwrap();
        assert!(e.enabled && e.rule.messages && e.rule.notices && e.rule.ctcp);
        assert_eq!(e.rule.mask, "bad!*@*");
    }

    #[test]
    fn add_remove_find_and_enabled_filtering() {
        let mut l = Vec::new();
        assert!(add(&mut l, Entry::new("Troll")));
        assert!(!add(&mut l, Entry::new("troll")), "same mask and scope");
        let mut scoped = Entry::new("troll");
        scoped.rule.channels = vec!["#x".into()];
        assert!(add(&mut l, scoped), "a channel-limited entry is a different scope");
        assert!(add(&mut l, Entry::new("*!*@evil.host")));
        assert_eq!(l.len(), 3);
        assert_eq!(find_nick(&l, "TROLL"), Some(0));
        assert_eq!(find_nick(&l, "nobody"), None);

        l[2].enabled = false;
        assert_eq!(rules(&l).len(), 2);

        assert_eq!(remove(&mut l, "troll"), 2, "removes every entry with that mask");
        assert_eq!(remove(&mut l, "*@evil.host"), 1, "partial masks are normalised before comparing");
        assert!(l.is_empty());
    }

    #[test]
    fn descriptions_are_readable() {
        let mut e = Entry::new("troll");
        assert_eq!(e.describe(), "[x] troll!*@*   (messages, notices, CTCP)");
        e.enabled = false;
        e.rule.ctcp = false;
        e.rule.channels = vec!["#a".into()];
        e.rule.network = Some("h".into());
        e.notes = "spam".into();
        assert_eq!(e.describe(), "[ ] troll!*@*   (messages, notices) in #a on h \u{2014} spam");
    }
}
