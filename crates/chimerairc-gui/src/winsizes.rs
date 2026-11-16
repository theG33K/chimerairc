//! Remembered places and sizes of chat windows (channels, queries, server windows, DCC chats): where a
//! window was when it was closed, so that it comes back the same way. Pure data, unit-tested.
//!
//! The key is the kind of window plus its lower-cased name, the same for every network. Settings
//! format, one line per window: `key<TAB>left<TAB>top<TAB>width<TAB>height<TAB>maximized`.

/// Most windows remembered; the least recently closed are forgotten first.
pub const MAX_ENTRIES: usize = 200;
const MIN_W: i32 = 200;
const MIN_H: i32 = 120;
const LIMIT: i32 = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub key: String,
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

/// The key for a window: `kind` is a short tag (`ch`, `q`, `srv`, `dcc`).
pub fn key(kind: &str, name: &str) -> String {
    let n: String = name.chars().filter(|c| !c.is_control() && *c != '\t').flat_map(|c| c.to_lowercase()).take(60).collect();
    format!("{kind}:{n}")
}

/// Remember a window, as the most recent one; an older entry for the same window is replaced.
pub fn remember(list: &mut Vec<Saved>, s: Saved) {
    if s.key.is_empty() || s.width < MIN_W || s.height < MIN_H {
        return;
    }
    list.retain(|e| e.key != s.key);
    list.push(s);
    while list.len() > MAX_ENTRIES {
        list.remove(0);
    }
}

pub fn find<'a>(list: &'a [Saved], key: &str) -> Option<&'a Saved> {
    list.iter().find(|e| e.key == key)
}

/// Make a remembered place usable in a work area of `area_w` x `area_h` pixels: not bigger than the
/// area, not tiny, and entirely inside it (the area may be smaller now than it was).
pub fn fit(s: &Saved, area_w: i32, area_h: i32) -> (i32, i32, i32, i32) {
    let w = s.width.clamp(MIN_W, area_w.max(MIN_W));
    let h = s.height.clamp(MIN_H, area_h.max(MIN_H));
    let l = s.left.clamp(0, (area_w - w).max(0));
    let t = s.top.clamp(0, (area_h - h).max(0));
    (l, t, w, h)
}

impl Saved {
    pub fn to_setting(&self) -> String {
        format!("{}\t{}\t{}\t{}\t{}\t{}", self.key, self.left, self.top, self.width, self.height, u8::from(self.maximized))
    }

    pub fn from_setting(v: &str) -> Option<Saved> {
        let mut p = v.split('\t');
        let key = p.next()?.to_owned();
        let mut num = || p.next()?.trim().parse::<i32>().ok().filter(|n| n.abs() <= LIMIT);
        let (left, top, width, height) = (num()?, num()?, num()?, num()?);
        let maximized = match p.next()?.trim() {
            "1" => true,
            "0" => false,
            _ => return None,
        };
        (!key.is_empty() && width >= MIN_W && height >= MIN_H).then_some(Saved { key, left, top, width, height, maximized })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(key: &str, l: i32, t: i32, w: i32, h: i32) -> Saved {
        Saved { key: key.into(), left: l, top: t, width: w, height: h, maximized: false }
    }

    #[test]
    fn keys_are_per_kind_and_ignore_case_and_control_characters() {
        assert_eq!(key("ch", "#Rust"), "ch:#rust");
        assert_eq!(key("q", "Bob"), "q:bob");
        assert_ne!(key("ch", "x"), key("q", "x"));
        assert_eq!(key("ch", "#a\tb\nc"), "ch:#abc");
        assert_eq!(key("ch", &"x".repeat(200)).len(), 3 + 60);
    }

    #[test]
    fn the_most_recent_entry_wins_and_the_oldest_are_forgotten() {
        let mut l = Vec::new();
        remember(&mut l, s("a", 0, 0, 300, 200));
        remember(&mut l, s("b", 0, 0, 300, 200));
        remember(&mut l, s("a", 10, 10, 400, 300));
        assert_eq!(l.len(), 2);
        assert_eq!(find(&l, "a").unwrap().width, 400);
        assert_eq!(l.last().unwrap().key, "a", "most recent last");
        for i in 0..MAX_ENTRIES + 50 {
            remember(&mut l, s(&format!("k{i}"), 0, 0, 300, 200));
        }
        assert_eq!(l.len(), MAX_ENTRIES);
        assert!(find(&l, "a").is_none(), "the oldest went first");
        assert!(find(&l, &format!("k{}", MAX_ENTRIES + 49)).is_some());
    }

    #[test]
    fn tiny_or_nameless_windows_are_not_remembered() {
        let mut l = Vec::new();
        remember(&mut l, s("a", 0, 0, 50, 50));
        remember(&mut l, s("", 0, 0, 300, 200));
        assert!(l.is_empty());
    }

    #[test]
    fn a_remembered_place_is_fitted_into_the_work_area() {
        let a = s("a", 100, 50, 400, 300);
        assert_eq!(fit(&a, 1000, 700), (100, 50, 400, 300), "fine as it is");
        assert_eq!(fit(&s("a", 900, 600, 400, 300), 1000, 700), (600, 400, 400, 300), "pulled back inside");
        assert_eq!(fit(&s("a", -80, -20, 400, 300), 1000, 700), (0, 0, 400, 300));
        assert_eq!(fit(&s("a", 0, 0, 3000, 2000), 800, 500), (0, 0, 800, 500), "never bigger than the area");
        let (_, _, w, h) = fit(&s("a", 0, 0, 300, 200), 100, 60);
        assert_eq!((w, h), (200, 120), "an absurdly small area still gets a usable window");
    }

    #[test]
    fn the_settings_format_roundtrips_and_junk_is_refused() {
        let w = Saved { maximized: true, ..s("ch:#rust", -5, 12, 640, 480) };
        assert_eq!(Saved::from_setting(&w.to_setting()), Some(w));
        for junk in ["", "k", "k\t1\t2\t3\t4", "k\t1\t2\t300\t200\t2", "k\ta\t2\t300\t200\t0", "\t1\t2\t300\t200\t0", "k\t1\t2\t50\t200\t0", "k\t999999\t2\t300\t200\t0"] {
            assert_eq!(Saved::from_setting(junk), None, "{junk:?}");
        }
    }
}
