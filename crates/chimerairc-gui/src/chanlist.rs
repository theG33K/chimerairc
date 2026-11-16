//! Data behind the Channel List window: the entries from a `LIST` reply plus the current
//! filter and sort. Pure logic (no Win32). Big networks return tens of thousands of channels,
//! so entries keep a pre-lowercased search key and the visible rows are just indices.

use chimerairc_proto::format;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub users: u32,
    /// Topic with IRC formatting codes removed.
    pub topic: String,
    /// `name\ntopic`, lower-cased once, for filtering.
    key: String,
}

impl Entry {
    pub fn new(name: &str, users: u32, topic: &str) -> Entry {
        let topic = format::strip(topic);
        let key = format!("{}\n{}", name.to_lowercase(), topic.to_lowercase());
        Entry { name: name.to_owned(), users, topic, key }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Name,
    Users,
    Topic,
}

pub struct ChannelList {
    entries: Vec<Entry>,
    /// Indices into `entries` that pass the filter, in display order.
    view: Vec<usize>,
    search: Vec<String>,
    min_users: u32,
    sort: Column,
    ascending: bool,
}

impl Default for ChannelList {
    fn default() -> Self {
        // Busiest channels first is what people want from a list of channels.
        ChannelList { entries: Vec::new(), view: Vec::new(), search: Vec::new(), min_users: 0, sort: Column::Users, ascending: false }
    }
}

impl ChannelList {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.view.clear();
    }

    pub fn push(&mut self, e: Entry) {
        self.entries.push(e);
    }

    /// Total entries received, regardless of the filter.
    pub fn total(&self) -> usize {
        self.entries.len()
    }

    /// Rows currently visible (after the last [`rebuild`](Self::rebuild)).
    pub fn visible(&self) -> usize {
        self.view.len()
    }

    pub fn get(&self, row: usize) -> Option<&Entry> {
        self.view.get(row).and_then(|i| self.entries.get(*i))
    }

    pub fn sort(&self) -> (Column, bool) {
        (self.sort, self.ascending)
    }

    /// Words are ANDed; each may match the channel name or the topic, case-insensitively.
    pub fn set_filter(&mut self, search: &str, min_users: u32) {
        self.search = search.split_whitespace().map(str::to_lowercase).collect();
        self.min_users = min_users;
    }

    /// Clicking the sorted column flips its direction; a new column starts at its natural order
    /// (users: biggest first, name/topic: A to Z).
    pub fn sort_by(&mut self, col: Column) {
        if self.sort == col {
            self.ascending = !self.ascending;
        } else {
            self.sort = col;
            self.ascending = col != Column::Users;
        }
    }

    /// Recompute the visible rows from the current filter and sort.
    pub fn rebuild(&mut self) {
        let (search, min) = (&self.search, self.min_users);
        let entries = &self.entries;
        let mut view: Vec<usize> = (0..entries.len())
            .filter(|&i| {
                let e = &entries[i];
                e.users >= min && search.iter().all(|w| e.key.contains(w.as_str()))
            })
            .collect();
        match self.sort {
            Column::Users => view.sort_by(|&a, &b| {
                entries[a].users.cmp(&entries[b].users).then_with(|| entries[b].name.to_lowercase().cmp(&entries[a].name.to_lowercase()))
            }),
            Column::Name => view.sort_by_cached_key(|&i| entries[i].name.to_lowercase()),
            Column::Topic => view.sort_by_cached_key(|&i| (entries[i].topic.to_lowercase(), entries[i].name.to_lowercase())),
        }
        if !self.ascending {
            view.reverse();
        }
        self.view = view;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ChannelList {
        let mut l = ChannelList::default();
        l.push(Entry::new("#rust", 1200, "The \x02Rust\x02 programming language"));
        l.push(Entry::new("#python", 3400, "Python help"));
        l.push(Entry::new("#linux", 3400, "Linux chat, not about rust"));
        l.push(Entry::new("#empty", 2, ""));
        l.push(Entry::new("#Zeta", 50, "zeta topic"));
        l
    }

    fn names(l: &ChannelList) -> Vec<String> {
        (0..l.visible()).map(|i| l.get(i).unwrap().name.clone()).collect()
    }

    #[test]
    fn default_is_biggest_first_with_stable_name_tiebreak() {
        let mut l = sample();
        l.rebuild();
        // 3400 twice: ties break by name A-Z even though the order is descending by users
        assert_eq!(names(&l), ["#linux", "#python", "#rust", "#Zeta", "#empty"]);
        assert_eq!(l.total(), 5);
    }

    #[test]
    fn filter_matches_name_or_topic_case_insensitively_and_ands_words() {
        let mut l = sample();
        l.set_filter("RUST", 0);
        l.rebuild();
        assert_eq!(names(&l), ["#linux", "#rust"]); // #linux matches via its topic
        l.set_filter("rust language", 0);
        l.rebuild();
        assert_eq!(names(&l), ["#rust"]);
        l.set_filter("nomatch", 0);
        l.rebuild();
        assert_eq!(l.visible(), 0);
        assert_eq!(l.total(), 5);
    }

    #[test]
    fn min_users_filter() {
        let mut l = sample();
        l.set_filter("", 1000);
        l.rebuild();
        assert_eq!(l.visible(), 3);
        l.set_filter("", 0);
        l.rebuild();
        assert_eq!(l.visible(), 5);
    }

    #[test]
    fn formatting_is_stripped_from_topics() {
        let l = sample();
        assert_eq!(l.get_unfiltered("#rust"), "The Rust programming language");
    }

    #[test]
    fn sorting_toggles_direction_on_repeat_clicks() {
        let mut l = sample();
        l.sort_by(Column::Name);
        l.rebuild();
        assert_eq!(names(&l), ["#empty", "#linux", "#python", "#rust", "#Zeta"]);
        l.sort_by(Column::Name);
        l.rebuild();
        assert_eq!(names(&l), ["#Zeta", "#rust", "#python", "#linux", "#empty"]);
        l.sort_by(Column::Users); // new column: natural order (biggest first)
        l.rebuild();
        assert_eq!(l.get(0).unwrap().users, 3400);
        l.sort_by(Column::Users);
        l.rebuild();
        assert_eq!(l.get(0).unwrap().users, 2);
        l.sort_by(Column::Topic);
        l.rebuild();
        assert_eq!(l.get(0).unwrap().name, "#empty"); // empty topic sorts first A-Z
    }

    #[test]
    fn clear_resets_everything() {
        let mut l = sample();
        l.rebuild();
        l.clear();
        assert_eq!((l.total(), l.visible()), (0, 0));
        assert!(l.get(0).is_none());
    }

    impl ChannelList {
        fn get_unfiltered(&self, name: &str) -> String {
            self.entries.iter().find(|e| e.name == name).unwrap().topic.clone()
        }
    }
}
