//! The filter box of list panes: which rows match what was typed. Pure logic, unit-tested.
//!
//! The text is split into words; a row matches when every word appears (in any order, ignoring case)
//! in at least one of its cells. An empty filter matches every row.

/// The words of a filter, lower-cased.
pub fn terms(filter: &str) -> Vec<String> {
    filter.split_whitespace().map(|w| w.to_lowercase()).collect()
}

/// Does a row (its cells) match the words?
pub fn matches(cells: &[String], terms: &[String]) -> bool {
    terms.iter().all(|t| cells.iter().any(|c| c.to_lowercase().contains(t.as_str())))
}

/// Which rows of a pane show for a filter, as indices into `cells`. Rows marked in `headers` are group
/// headings (as in the Buddies pane): a heading shows when it matches itself (and then so do all the rows
/// under it) or when any row under it matches (and then only the matching rows are listed under it).
pub fn visible(cells: &[&[String]], headers: &[bool], terms: &[String]) -> Vec<usize> {
    let n = cells.len();
    if terms.is_empty() {
        return (0..n).collect();
    }
    let is_header = |i: usize| headers.get(i).copied().unwrap_or(false);
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if is_header(i) {
            let mut end = i + 1;
            while end < n && !is_header(end) {
                end += 1;
            }
            let own = matches(cells[i], terms);
            let members: Vec<usize> = (i + 1..end).filter(|k| own || matches(cells[*k], terms)).collect();
            if own || !members.is_empty() {
                out.push(i);
                out.extend(members);
            }
            i = end;
        } else {
            if matches(cells[i], terms) {
                out.push(i);
            }
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(a: &str, b: &str, c: &str) -> Vec<String> {
        vec![a.into(), b.into(), c.into()]
    }

    #[test]
    fn an_empty_filter_matches_everything() {
        assert!(terms("").is_empty() && terms("   \t ").is_empty());
        assert!(matches(&row("alice", "irc.example.net", "#a"), &[]));
        assert!(matches(&[], &[]));
    }

    #[test]
    fn a_word_may_be_found_in_any_cell_ignoring_case() {
        let r = row("Alice", "irc.Example.net", "#rust, #linux");
        for t in ["ali", "ALICE", "example", "#rust", "linux", "e.net"] {
            assert!(matches(&r, &terms(t)), "{t}");
        }
        for t in ["bob", "#python", "alice2"] {
            assert!(!matches(&r, &terms(t)), "{t}");
        }
    }

    #[test]
    fn every_word_has_to_match_but_not_in_the_same_cell_or_order() {
        let r = row("alice", "irc.example.net", "#rust, #linux");
        assert!(matches(&r, &terms("alice #rust")));
        assert!(matches(&r, &terms("#rust   alice")), "order and spacing do not matter");
        assert!(matches(&r, &terms("example linux")));
        assert!(!matches(&r, &terms("alice #python")), "one missing word is enough to reject");
    }

    fn rows(list: &[&[&str]]) -> Vec<Vec<String>> {
        list.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect()
    }

    fn view(list: &[&[&str]], headers: &[bool], filter: &str) -> Vec<usize> {
        let owned = rows(list);
        let cells: Vec<&[String]> = owned.iter().map(|r| r.as_slice()).collect();
        visible(&cells, headers, &terms(filter))
    }

    #[test]
    fn a_plain_list_shows_the_matching_rows_in_order() {
        let list: &[&[&str]] = &[&["alice", "a"], &["bob", "b"], &["carol", "a"]];
        assert_eq!(view(list, &[], ""), [0, 1, 2], "no filter: everything");
        assert_eq!(view(list, &[], "a"), [0, 2], "bob has no a anywhere");
        assert_eq!(view(list, &[], "carol"), [2]);
        assert_eq!(view(list, &[], "zzz"), Vec::<usize>::new());
        assert_eq!(view(&[], &[], "x"), Vec::<usize>::new());
    }

    #[test]
    fn a_group_heading_shows_with_its_matching_members_only() {
        // [Work] alice carol   [Pals] dave
        let list: &[&[&str]] = &[&["Work", "1/2"], &["alice", "Work"], &["carol", "Work"], &["Pals", "0/1"], &["dave", "Pals"]];
        let h = [true, false, false, true, false];
        assert_eq!(view(list, &h, "carol"), [0, 2], "the heading comes along, the other member does not");
        assert_eq!(view(list, &h, "dave"), [3, 4]);
        assert_eq!(view(list, &h, "nobody"), Vec::<usize>::new());
    }

    #[test]
    fn a_matching_heading_brings_all_its_members() {
        let list: &[&[&str]] = &[&["Work", "1/2"], &["alice", "x"], &["carol", "x"], &["Pals", "0/1"], &["dave", "y"]];
        let h = [true, false, false, true, false];
        assert_eq!(view(list, &h, "work"), [0, 1, 2]);
        // a folded group is just its heading, which can match on its own
        let folded: &[&[&str]] = &[&["Work", "0/2"], &["Pals", "0/1"], &["dave", "y"]];
        assert_eq!(view(folded, &[true, true, false], "work"), [0]);
    }

    #[test]
    fn unicode_is_compared_case_insensitively_too() {
        let r = row("Åsa", "irc.example.net", "#sv");
        assert!(matches(&r, &terms("åsa")));
        assert!(matches(&r, &terms("ÅSA")));
    }
}
