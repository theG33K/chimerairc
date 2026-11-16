//! Helpers for chat history in the interface: which lines were already shown, the server's UTC
//! timestamps, and the `/history` command. Pure, unit-tested.

use std::collections::{HashSet, VecDeque};

/// How many message ids are remembered per connection.
const SEEN_LIMIT: usize = 5000;

/// Message ids already shown, so that history replayed after a reconnect (or fetched twice) does not
/// print the same line again. Old ids are forgotten first.
#[derive(Default)]
pub struct Seen {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl Seen {
    /// Remember `id`. True if it had not been seen before.
    pub fn mark(&mut self, id: &str) -> bool {
        if id.is_empty() || !self.set.insert(id.to_owned()) {
            return id.is_empty(); // a line without an id is never a repeat
        }
        self.order.push_back(id.to_owned());
        if self.order.len() > SEEN_LIMIT {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }

    #[cfg(test)]
    pub fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }
}

/// `2024-05-05T10:00:09.123Z` -> (2024, 5, 5, 10, 0, 9). Only UTC (`Z`) times, which is what the
/// `server-time` capability promises.
pub fn parse_iso(iso: &str) -> Option<(u16, u16, u16, u16, u16, u16)> {
    let iso = iso.strip_suffix('Z')?;
    let (date, time) = iso.split_once('T')?;
    let mut d = date.split('-');
    let (y, mo, da) = (d.next()?, d.next()?, d.next()?);
    if d.next().is_some() {
        return None;
    }
    let time = time.split('.').next()?;
    let mut t = time.split(':');
    let (h, mi, s) = (t.next()?, t.next()?, t.next()?);
    if t.next().is_some() || y.len() != 4 || mo.len() != 2 || da.len() != 2 || h.len() != 2 || mi.len() != 2 || s.len() != 2 {
        return None;
    }
    let n = |x: &str| x.parse::<u16>().ok();
    let (y, mo, da, h, mi, s) = (n(y)?, n(mo)?, n(da)?, n(h)?, n(mi)?, n(s)?);
    let ok = (1970..=9999).contains(&y) && (1..=12).contains(&mo) && (1..=31).contains(&da) && h < 24 && mi < 60 && s < 61;
    ok.then_some((y, mo, da, h, mi, s))
}

/// Whether the server timestamp `a` is strictly earlier than `b` (both valid ISO UTC times, which
/// sort correctly as text).
pub fn earlier(a: &str, b: &str) -> bool {
    parse_iso(a).is_some() && parse_iso(b).is_some() && a < b
}

/// What a `/history` command asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// The newest `n` lines.
    Latest(u32),
    /// `n` lines older than the point given (`timestamp=...` or `msgid=...`), or older than the oldest one
    /// already shown when none is given.
    Before(Option<String>, u32),
    /// `n` lines newer than the point given, or newer than the newest one already shown.
    After(Option<String>, u32),
    /// About `n` lines around one point.
    Around(String, u32),
    /// The lines between two points.
    Between(String, String, u32),
}

pub const DEFAULT_LINES: u32 = 50;
pub const MAX_LINES: u32 = 500;

const USAGE: &str = "Usage: /history [lines] | before|after [point] [lines] | around <point> [lines] | between <point> <point> [lines]   (a point is a UTC time like 2024-05-05T10:00:00Z or msgid=ID)";

/// A point in the history as the server wants it: `timestamp=...` or `msgid=...`.
pub fn parse_point(word: &str) -> Option<String> {
    let id = word.strip_prefix("msgid=").or_else(|| word.strip_prefix("msgid:"));
    if let Some(id) = id {
        let ok = !id.is_empty() && id.len() <= 100 && id.chars().all(|c| c.is_ascii_graphic() && c != ',');
        return ok.then(|| format!("msgid={id}"));
    }
    let time = word.strip_prefix("timestamp=").unwrap_or(word);
    parse_iso(time).map(|_| format!("timestamp={time}"))
}

fn parse_lines(word: &str) -> Option<u32> {
    word.parse::<u32>().ok().filter(|v| *v > 0).map(|v| v.min(MAX_LINES))
}

/// Parse the arguments of `/history`. With nothing, or a number, the newest lines; `before` (also `older`,
/// `more`, `back`) and `after` (`newer`) step back or forward from what is shown or from a point; `around`
/// and `between` ask for a window of history. A number at the end is the line count.
pub fn parse_command(args: &str) -> Result<Request, String> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let usage = || Err(USAGE.to_string());
    let Some((first, rest)) = words.split_first() else { return Ok(Request::Latest(DEFAULT_LINES)) };
    let word = first.to_ascii_lowercase();
    // the common case: a count alone
    if let Some(n) = parse_lines(&word) {
        return if rest.is_empty() { Ok(Request::Latest(n)) } else { usage() };
    }
    // the optional trailing count
    let (points, n) = match rest.split_last() {
        Some((last, before)) if parse_lines(last).is_some() => (before, parse_lines(last).unwrap()),
        _ => (rest, DEFAULT_LINES),
    };
    match (word.as_str(), points) {
        ("before" | "older" | "more" | "back", []) => Ok(Request::Before(None, n)),
        ("before" | "older" | "more" | "back", [p]) => parse_point(p).map(|p| Request::Before(Some(p), n)).ok_or_else(|| USAGE.to_string()),
        ("after" | "newer", []) => Ok(Request::After(None, n)),
        ("after" | "newer", [p]) => parse_point(p).map(|p| Request::After(Some(p), n)).ok_or_else(|| USAGE.to_string()),
        ("around", [p]) => parse_point(p).map(|p| Request::Around(p, n)).ok_or_else(|| USAGE.to_string()),
        ("between", [a, b]) => match (parse_point(a), parse_point(b)) {
            (Some(a), Some(b)) => Ok(Request::Between(a, b, n)),
            _ => usage(),
        },
        _ => usage(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seen_ids_are_remembered_and_old_ones_forgotten() {
        let mut s = Seen::default();
        assert!(s.mark("a") && s.mark("b"));
        assert!(!s.mark("a"), "a repeat");
        assert!(s.contains("a") && !s.contains("zzz"));
        assert!(s.mark("") && s.mark(""), "lines without an id are never treated as repeats");
        for i in 0..SEEN_LIMIT + 10 {
            s.mark(&format!("id{i}"));
        }
        assert!(!s.contains("a"), "the oldest ids drop out");
        assert!(s.contains(&format!("id{}", SEEN_LIMIT + 9)));
        assert!(s.order.len() <= SEEN_LIMIT && s.set.len() <= SEEN_LIMIT);
    }

    #[test]
    fn server_times_are_parsed_strictly() {
        assert_eq!(parse_iso("2024-05-05T10:00:09.123Z"), Some((2024, 5, 5, 10, 0, 9)));
        assert_eq!(parse_iso("2024-12-31T23:59:59Z"), Some((2024, 12, 31, 23, 59, 59)));
        for bad in [
            "", "garbage", "2024-05-05 10:00:09Z", "2024-05-05T10:00:09", "2024-05-05T10:00:09+02:00", "2024-13-05T10:00:09Z",
            "2024-05-32T10:00:09Z", "2024-05-05T25:00:09Z", "2024-05-05T10:61:09Z", "24-05-05T10:00:09Z", "2024-5-5T1:0:9Z",
            "2024-05-05-01T10:00:09Z", "2024-05-05T10:00:09:11Z", "1969-12-31T23:59:59Z", "20xx-05-05T10:00:09Z",
        ] {
            assert_eq!(parse_iso(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn earlier_compares_valid_times_only() {
        assert!(earlier("2024-05-05T10:00:00.000Z", "2024-05-05T10:00:01.000Z"));
        assert!(!earlier("2024-05-05T10:00:01.000Z", "2024-05-05T10:00:00.000Z"));
        assert!(!earlier("2024-05-05T10:00:00.000Z", "2024-05-05T10:00:00.000Z"));
        assert!(!earlier("junk", "2024-05-05T10:00:00.000Z"));
        assert!(!earlier("2024-05-05T10:00:00.000Z", "junk"));
        assert!(earlier("2023-12-31T23:59:59.999Z", "2024-01-01T00:00:00.000Z"));
    }

    #[test]
    fn the_history_command_is_parsed() {
        assert_eq!(parse_command(""), Ok(Request::Latest(DEFAULT_LINES)));
        assert_eq!(parse_command("20"), Ok(Request::Latest(20)));
        assert_eq!(parse_command("before"), Ok(Request::Before(None, DEFAULT_LINES)));
        assert_eq!(parse_command("BEFORE 10"), Ok(Request::Before(None, 10)));
        assert_eq!(parse_command("more 5"), Ok(Request::Before(None, 5)));
        assert_eq!(parse_command("100000"), Ok(Request::Latest(MAX_LINES)), "capped");
        for bad in ["0", "-5", "abc", "before before", "10 20", "before 10 20", "10 before", "5.5"] {
            assert!(parse_command(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn history_can_go_forward_around_and_between_points() {
        assert_eq!(parse_command("after"), Ok(Request::After(None, DEFAULT_LINES)));
        assert_eq!(parse_command("newer 7"), Ok(Request::After(None, 7)));
        assert_eq!(parse_command("before 2024-05-05T10:00:00Z 20"), Ok(Request::Before(Some("timestamp=2024-05-05T10:00:00Z".into()), 20)));
        assert_eq!(parse_command("after msgid=abc123"), Ok(Request::After(Some("msgid=abc123".into()), DEFAULT_LINES)));
        assert_eq!(parse_command("around msgid:m1 10"), Ok(Request::Around("msgid=m1".into(), 10)));
        assert_eq!(
            parse_command("between 2024-05-05T10:00:00.000Z msgid=zz 30"),
            Ok(Request::Between("timestamp=2024-05-05T10:00:00.000Z".into(), "msgid=zz".into(), 30))
        );
        for bad in ["around", "around 20", "between 2024-05-05T10:00:00Z", "between a b", "after nonsense", "around msgid=", "before 2024-13-05T10:00:00Z", "latest x"] {
            assert!(parse_command(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn points_are_checked_before_they_go_into_a_command() {
        assert_eq!(parse_point("2024-05-05T10:00:00Z").as_deref(), Some("timestamp=2024-05-05T10:00:00Z"));
        assert_eq!(parse_point("timestamp=2024-05-05T10:00:00Z").as_deref(), Some("timestamp=2024-05-05T10:00:00Z"));
        assert_eq!(parse_point("msgid=a-b_c").as_deref(), Some("msgid=a-b_c"));
        for bad in ["", "msgid=", "msgid=a,b", "yesterday", "2024-05-05", "msgid=a\u{7}b"] {
            assert_eq!(parse_point(bad), None, "{bad:?}");
        }
    }
}
