//! The buddy list: people whose coming and going you want to see. Pure data, unit-tested; the
//! settings file stores it and `app.rs` keeps the servers informed (MONITOR / ISON).
//!
//! Settings format, one line per buddy: `nick<TAB>group<TAB>note`.

use chimerairc_core::presence::{valid_nick, MAX_WATCH};

const MAX_GROUP: usize = 30;
const MAX_NOTE: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buddy {
    pub nick: String,
    /// Free-form label buddies are sorted under; may be empty.
    pub group: String,
    pub note: String,
}

fn clean(s: &str, max: usize) -> String {
    let s: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    s.trim().chars().take(max).collect()
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Why a buddy could not be added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddError {
    BadNick,
    Duplicate,
    Full,
}

impl AddError {
    pub fn text(self) -> &'static str {
        match self {
            AddError::BadNick => "that is not a valid nickname",
            AddError::Duplicate => "already on the buddy list",
            AddError::Full => "the buddy list is full",
        }
    }
}

pub fn add(list: &mut Vec<Buddy>, nick: &str, group: &str, note: &str) -> Result<(), AddError> {
    let nick = nick.trim();
    if !valid_nick(nick) {
        return Err(AddError::BadNick);
    }
    if list.iter().any(|b| same(&b.nick, nick)) {
        return Err(AddError::Duplicate);
    }
    if list.len() >= MAX_WATCH {
        return Err(AddError::Full);
    }
    list.push(Buddy { nick: nick.to_owned(), group: clean(group, MAX_GROUP), note: clean(note, MAX_NOTE) });
    Ok(())
}

pub fn remove(list: &mut Vec<Buddy>, nick: &str) -> bool {
    let before = list.len();
    list.retain(|b| !same(&b.nick, nick.trim()));
    list.len() != before
}

pub fn find<'a>(list: &'a [Buddy], nick: &str) -> Option<&'a Buddy> {
    list.iter().find(|b| same(&b.nick, nick))
}

/// The nicknames to watch.
pub fn nicks(list: &[Buddy]) -> Vec<String> {
    list.iter().map(|b| b.nick.clone()).collect()
}

/// The buddies in display order: online ones first, then by group (ungrouped last) and nick.
pub fn display_order<'a>(list: &'a [Buddy], online: &dyn Fn(&str) -> bool) -> Vec<&'a Buddy> {
    let mut v: Vec<&Buddy> = list.iter().collect();
    v.sort_by_cached_key(|b| (!online(&b.nick), b.group.is_empty(), b.group.to_lowercase(), b.nick.to_lowercase()));
    v
}

/// The key under which the buddies without a group are remembered as collapsed.
pub const UNGROUPED: &str = "(none)";

/// The key of a group (lower case; the empty group has its own).
pub fn group_key(group: &str) -> String {
    if group.trim().is_empty() {
        UNGROUPED.to_owned()
    } else {
        group.trim().to_lowercase()
    }
}

/// One line of the Buddies pane.
#[derive(Debug, PartialEq, Eq)]
pub enum Line<'a> {
    /// A group's heading with how many of its buddies are online, and whether it is folded up.
    Header { key: String, title: &'a str, online: usize, total: usize, collapsed: bool },
    Buddy(&'a Buddy),
}

/// What the Buddies pane shows. With no groups in use it is just the buddies, online ones first. With
/// groups, every group (alphabetically, buddies without one last) gets a heading and then its buddies,
/// online ones first, unless the group is in `collapsed` (a list of group keys).
pub fn lines<'a>(list: &'a [Buddy], online: &dyn Fn(&str) -> bool, collapsed: &[String]) -> Vec<Line<'a>> {
    if list.iter().all(|b| b.group.trim().is_empty()) {
        return display_order(list, online).into_iter().map(Line::Buddy).collect();
    }
    let mut keys: Vec<String> = list.iter().map(|b| group_key(&b.group)).collect();
    keys.sort_by_key(|k| (k == UNGROUPED, k.clone()));
    keys.dedup();
    let mut out = Vec::new();
    for key in keys {
        let mut members: Vec<&Buddy> = list.iter().filter(|b| group_key(&b.group) == key).collect();
        members.sort_by_cached_key(|b| (!online(&b.nick), b.nick.to_lowercase()));
        let title = members.iter().map(|b| b.group.trim()).find(|g| !g.is_empty()).unwrap_or("No group");
        let folded = collapsed.contains(&key);
        out.push(Line::Header { online: members.iter().filter(|b| online(&b.nick)).count(), total: members.len(), title, collapsed: folded, key });
        if !folded {
            out.extend(members.into_iter().map(Line::Buddy));
        }
    }
    out
}

/// Fold a group up, or unfold it. Returns true when it is folded afterwards.
pub fn toggle_collapsed(collapsed: &mut Vec<String>, key: &str) -> bool {
    if let Some(i) = collapsed.iter().position(|k| k == key) {
        collapsed.remove(i);
        false
    } else {
        collapsed.push(key.to_owned());
        true
    }
}

/// The keys of every group in use (for "collapse all").
pub fn all_group_keys(list: &[Buddy]) -> Vec<String> {
    let mut keys: Vec<String> = list.iter().map(|b| group_key(&b.group)).collect();
    keys.sort();
    keys.dedup();
    keys
}

impl Buddy {
    pub fn to_setting(&self) -> String {
        format!("{}\t{}\t{}", self.nick, self.group, self.note)
    }

    pub fn from_setting(v: &str) -> Option<Buddy> {
        let mut p = v.split('\t');
        let nick = p.next()?.trim();
        if !valid_nick(nick) {
            return None;
        }
        Some(Buddy { nick: nick.to_owned(), group: clean(p.next().unwrap_or(""), MAX_GROUP), note: clean(p.next().unwrap_or(""), MAX_NOTE) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Vec<Buddy> {
        let mut l = Vec::new();
        add(&mut l, "Zed", "", "").unwrap();
        add(&mut l, "alice", "Work", "").unwrap();
        add(&mut l, "Bob", "friends", "met on #rust").unwrap();
        add(&mut l, "carol", "Work", "").unwrap();
        l
    }

    #[test]
    fn adding_checks_the_nick_and_refuses_duplicates() {
        let mut l = list();
        assert_eq!(add(&mut l, "ALICE", "", ""), Err(AddError::Duplicate), "case-insensitive");
        assert_eq!(add(&mut l, "a b", "", ""), Err(AddError::BadNick));
        assert_eq!(add(&mut l, "#chan", "", ""), Err(AddError::BadNick));
        assert_eq!(add(&mut l, "  dave ", "", ""), Ok(()));
        assert_eq!(l.last().unwrap().nick, "dave", "trimmed");
        assert_eq!(l.len(), 5);
    }

    #[test]
    fn the_list_has_a_ceiling() {
        let mut l = Vec::new();
        for i in 0..MAX_WATCH {
            add(&mut l, &format!("n{i}"), "", "").unwrap();
        }
        assert_eq!(add(&mut l, "one_more", "", ""), Err(AddError::Full));
    }

    #[test]
    fn removing_and_finding_ignore_case() {
        let mut l = list();
        assert!(find(&l, "BOB").is_some());
        assert!(remove(&mut l, " bob "));
        assert!(!remove(&mut l, "bob"));
        assert_eq!(l.len(), 3);
        assert_eq!(nicks(&l), ["Zed", "alice", "carol"]);
    }

    #[test]
    fn online_buddies_come_first_then_groups_then_names() {
        let l = list();
        let on = |n: &str| n == "Zed" || n == "carol";
        let order: Vec<&str> = display_order(&l, &on).iter().map(|b| b.nick.as_str()).collect();
        // online: carol (group Work) before Zed (no group); offline: Bob (friends) before alice (Work)? groups sort
        // case-insensitively: friends < work
        assert_eq!(order, ["carol", "Zed", "Bob", "alice"]);
    }

    fn grouped() -> Vec<Buddy> {
        let mut l = Vec::new();
        add(&mut l, "zed", "", "").unwrap();
        add(&mut l, "alice", "Work", "").unwrap();
        add(&mut l, "bob", "friends", "").unwrap();
        add(&mut l, "carol", "work", "").unwrap();
        add(&mut l, "dave", "Work", "").unwrap();
        l
    }

    fn describe(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| match l {
                Line::Header { title, online, total, collapsed, .. } => format!("[{title} {online}/{total}{}]", if *collapsed { " folded" } else { "" }),
                Line::Buddy(b) => b.nick.clone(),
            })
            .collect()
    }

    #[test]
    fn without_groups_the_pane_is_a_flat_list_with_online_buddies_first() {
        let l = vec![Buddy { nick: "b".into(), group: "".into(), note: "".into() }, Buddy { nick: "a".into(), group: " ".into(), note: "".into() }];
        let on = |n: &str| n == "b";
        assert_eq!(describe(&lines(&l, &on, &[])), ["b", "a"]);
    }

    #[test]
    fn with_groups_each_gets_a_heading_and_its_buddies_online_first() {
        let l = grouped();
        let on = |n: &str| n == "dave" || n == "zed";
        let d = describe(&lines(&l, &on, &[]));
        assert_eq!(d, ["[friends 0/1]", "bob", "[Work 1/3]", "dave", "alice", "carol", "[No group 1/1]", "zed"]);
        // group names that differ only by case are one group
        assert_eq!(all_group_keys(&l), ["(none)", "friends", "work"]);
    }

    #[test]
    fn a_collapsed_group_shows_only_its_heading_and_its_count() {
        let l = grouped();
        let on = |n: &str| n == "dave";
        let d = describe(&lines(&l, &on, &["work".to_owned(), UNGROUPED.to_owned()]));
        assert_eq!(d, ["[friends 0/1]", "bob", "[Work 1/3 folded]", "[No group 0/1 folded]"]);
    }

    #[test]
    fn folding_toggles_and_remembers_keys() {
        let mut c = Vec::new();
        assert!(toggle_collapsed(&mut c, "work"));
        assert!(toggle_collapsed(&mut c, "friends"));
        assert!(!toggle_collapsed(&mut c, "work"));
        assert_eq!(c, ["friends"]);
        assert_eq!(group_key("  Work "), "work");
        assert_eq!(group_key(""), UNGROUPED);
    }

    #[test]
    fn settings_roundtrip_and_junk_is_handled() {
        for b in list() {
            assert_eq!(Buddy::from_setting(&b.to_setting()).as_ref(), Some(&b));
        }
        assert_eq!(Buddy::from_setting("solo").unwrap(), Buddy { nick: "solo".into(), group: "".into(), note: "".into() });
        for junk in ["", "\tgroup", "a b\tg", "#chan\t\t", "x,y"] {
            assert!(Buddy::from_setting(junk).is_none(), "{junk:?}");
        }
        // control characters are neutralised and lengths are capped
        let b = Buddy::from_setting(&format!("n\tg\r\nx\t{}", "y".repeat(500))).unwrap();
        assert_eq!(b.group, "g  x");
        assert_eq!(b.note.len(), MAX_NOTE);
    }
}
