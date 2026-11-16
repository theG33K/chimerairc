//! Remembered column widths of the list windows and panes (Messages, Buddies, the Channel List ...).
//! Pure data, unit-tested. Widths are in 96-dpi pixels, so they survive a change of display scaling.
//!
//! Settings format, one line per list: `key<TAB>w1,w2,w3`.

/// Most lists remembered.
const MAX_LISTS: usize = 32;
const MAX_COLUMNS: usize = 16;
const MIN_W: u32 = 10;
const MAX_W: u32 = 3000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Widths {
    pub key: String,
    pub widths: Vec<u32>,
}

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= 32 && k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

fn valid_widths(w: &[u32]) -> bool {
    !w.is_empty() && w.len() <= MAX_COLUMNS && w.iter().all(|x| (MIN_W..=MAX_W).contains(x))
}

/// The remembered widths of list `key`, if there are some for exactly `columns` columns (a list that
/// gained or lost a column starts from its defaults again).
pub fn get<'a>(list: &'a [Widths], key: &str, columns: usize) -> Option<&'a [u32]> {
    list.iter().find(|e| e.key == key && e.widths.len() == columns).map(|e| e.widths.as_slice())
}

/// Remember the widths of list `key`. Refused (false) for a bad key or nonsense widths.
pub fn set(list: &mut Vec<Widths>, key: &str, widths: &[u32]) -> bool {
    if !valid_key(key) || !valid_widths(widths) {
        return false;
    }
    match list.iter_mut().find(|e| e.key == key) {
        Some(e) => e.widths = widths.to_vec(),
        None => {
            if list.len() >= MAX_LISTS {
                return false;
            }
            list.push(Widths { key: key.to_owned(), widths: widths.to_vec() });
        }
    }
    true
}

impl Widths {
    pub fn to_setting(&self) -> String {
        let w: Vec<String> = self.widths.iter().map(|x| x.to_string()).collect();
        format!("{}\t{}", self.key, w.join(","))
    }

    pub fn from_setting(v: &str) -> Option<Widths> {
        let (key, rest) = v.split_once('\t')?;
        let widths: Vec<u32> = rest.split(',').map(|x| x.trim().parse().ok()).collect::<Option<_>>()?;
        (valid_key(key) && valid_widths(&widths)).then(|| Widths { key: key.to_owned(), widths })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_are_found_only_for_the_same_number_of_columns() {
        let mut l = Vec::new();
        assert!(set(&mut l, "dock.buddies", &[100, 60, 70, 160]));
        assert_eq!(get(&l, "dock.buddies", 4), Some(&[100, 60, 70, 160][..]));
        assert_eq!(get(&l, "dock.buddies", 3), None, "a list that changed shape starts over");
        assert_eq!(get(&l, "other", 4), None);
    }

    #[test]
    fn setting_again_replaces_and_never_duplicates() {
        let mut l = Vec::new();
        set(&mut l, "a", &[50, 60]);
        set(&mut l, "a", &[70, 80]);
        assert_eq!(l.len(), 1);
        assert_eq!(get(&l, "a", 2), Some(&[70, 80][..]));
    }

    #[test]
    fn nonsense_is_refused() {
        let mut l = Vec::new();
        assert!(!set(&mut l, "", &[50]));
        assert!(!set(&mut l, "Bad Key", &[50]));
        assert!(!set(&mut l, "a", &[]));
        assert!(!set(&mut l, "a", &[5]), "too narrow to be real");
        assert!(!set(&mut l, "a", &[5000]));
        assert!(!set(&mut l, "a", &[50; 17]));
        assert!(l.is_empty());
    }

    #[test]
    fn the_number_of_remembered_lists_is_capped() {
        let mut l = Vec::new();
        for i in 0..MAX_LISTS {
            assert!(set(&mut l, &format!("k{i}"), &[50]));
        }
        assert!(!set(&mut l, "one_more", &[50]));
        assert!(set(&mut l, "k0", &[99]), "existing lists can still be updated");
    }

    #[test]
    fn the_settings_format_roundtrips_and_junk_is_ignored() {
        let w = Widths { key: "chanlist".into(), widths: vec![170, 70, 480] };
        assert_eq!(Widths::from_setting(&w.to_setting()), Some(w));
        for junk in ["", "nokey", "a\t", "a\tx,y", "a\t50,,60", "Bad\t50", "a\t5", "a\t50,60\textra"] {
            assert_eq!(Widths::from_setting(junk), None, "{junk:?}");
        }
    }
}
