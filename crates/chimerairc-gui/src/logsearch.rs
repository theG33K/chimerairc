//! Searching the conversation logs (see `logger`): plain text, case-insensitive unless asked otherwise. Pure std,
//! unit-tested. The search is bounded: it reads at most the tail of each file, at most so many bytes in all, and
//! stops after so many hits, so a huge log folder cannot freeze the program for long.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// Where it was said: `irc.libera.chat #rust`, `irc.libera.chat status`, `irc.libera.chat alice`.
    pub place: String,
    /// The log line as written: `[2024-05-05 10:00:00] <alice> hello`.
    pub line: String,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub case_sensitive: bool,
    pub max_hits: usize,
    /// Most bytes read from one file (the end of a bigger file is searched).
    pub max_file_bytes: u64,
    /// Most bytes read in all.
    pub max_total_bytes: u64,
    pub max_files: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options { case_sensitive: false, max_hits: 300, max_file_bytes: 20 << 20, max_total_bytes: 400 << 20, max_files: 2000 }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub hits: Vec<Hit>,
    pub files_searched: usize,
    /// The search stopped early (too many hits, files or bytes).
    pub truncated: bool,
}

/// The shortest search text we accept.
pub const MIN_QUERY: usize = 2;

/// Every `.log` file below `base` (`<host>\status.log`, `<host>\channels\*.log`, `<host>\queries\*.log`), newest
/// first, as (path, label).
fn log_files(base: &Path, only_host: Option<&str>) -> Vec<(PathBuf, String, std::time::SystemTime)> {
    let mut out = Vec::new();
    let Ok(hosts) = fs::read_dir(base) else { return out };
    for host in hosts.flatten() {
        let hpath = host.path();
        let Some(hname) = hpath.file_name().and_then(|n| n.to_str()).map(str::to_owned) else { continue };
        if !hpath.is_dir() || only_host.is_some_and(|h| !hname.eq_ignore_ascii_case(&crate::logger::sanitize(h))) {
            continue;
        }
        let mut add = |p: PathBuf, what: String| {
            if let Ok(m) = p.metadata() {
                if m.is_file() {
                    out.push((p, format!("{hname} {what}"), m.modified().unwrap_or(std::time::UNIX_EPOCH)));
                }
            }
        };
        add(hpath.join("status.log"), "status".into());
        for sub in ["channels", "queries"] {
            if let Ok(rd) = fs::read_dir(hpath.join(sub)) {
                for f in rd.flatten() {
                    let p = f.path();
                    if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("log")) {
                        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("?").to_owned();
                        add(p, stem);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| b.2.cmp(&a.2));
    out
}

/// The lines of `path` (its last `max` bytes when it is bigger) that contain `needle`.
fn search_file(path: &Path, place: &str, needle: &str, opt: &Options, budget: &mut u64, out: &mut Vec<Hit>) -> std::io::Result<bool> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let take = len.min(opt.max_file_bytes).min(*budget);
    if take < len {
        f.seek(SeekFrom::Start(len - take))?;
    }
    *budget -= take;
    let mut reader = BufReader::new(f.take(take));
    let mut buf = Vec::new();
    let mut first = take < len; // the first line of a tail may be cut: skip it
    let mut full = false;
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        if first {
            first = false;
            continue;
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\r', '\n']);
        if line.starts_with("--- Log opened") {
            continue;
        }
        let hit = if opt.case_sensitive { line.contains(needle) } else { line.to_lowercase().contains(needle) };
        if hit {
            if out.len() >= opt.max_hits {
                full = true;
                break;
            }
            out.push(Hit { place: place.to_owned(), line: line.chars().take(500).collect() });
        }
    }
    Ok(full)
}

/// Search the logs under `base` for `query`. `only_host` limits it to one server's folder.
pub fn search(base: &Path, only_host: Option<&str>, query: &str, opt: &Options) -> Outcome {
    let query = query.trim();
    let mut outcome = Outcome::default();
    if query.chars().count() < MIN_QUERY {
        return outcome;
    }
    let needle = if opt.case_sensitive { query.to_owned() } else { query.to_lowercase() };
    let files = log_files(base, only_host);
    if files.len() > opt.max_files {
        outcome.truncated = true;
    }
    let mut budget = opt.max_total_bytes;
    for (path, place, _) in files.into_iter().take(opt.max_files) {
        if budget == 0 {
            outcome.truncated = true;
            break;
        }
        match search_file(&path, &place, &needle, opt, &mut budget, &mut outcome.hits) {
            Ok(full) => {
                outcome.files_searched += 1;
                if full {
                    outcome.truncated = true;
                    break;
                }
            }
            Err(_) => continue, // a file we cannot read is skipped
        }
    }
    outcome
}

/// The text of one result as it is shown.
pub fn hit_text(h: &Hit) -> String {
    format!("[{}] {}", h.place, h.line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("chimerairc-logsearch-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("irc.a.example").join("channels")).unwrap();
        fs::create_dir_all(d.join("irc.b.example").join("queries")).unwrap();
        d
    }

    fn write(p: &Path, text: &str) {
        let mut f = File::create(p).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn finds_lines_in_channels_queries_and_status_ignoring_case() {
        let d = temp("basic");
        write(&d.join("irc.a.example").join("channels").join("#rust.log"), "--- Log opened 2024-05-05 ---\n[2024-05-05 10:00:00] <alice> Hello World\n[2024-05-05 10:00:01] <bob> nothing\n");
        write(&d.join("irc.a.example").join("status.log"), "[2024-05-05 09:00:00] * hello from the server\n");
        write(&d.join("irc.b.example").join("queries").join("carol.log"), "[2024-05-05 11:00:00] <carol> HELLO there\n");
        let r = search(&d, None, "hello", &Options::default());
        assert_eq!(r.hits.len(), 3);
        assert_eq!(r.files_searched, 3);
        assert!(!r.truncated);
        let places: Vec<&str> = r.hits.iter().map(|h| h.place.as_str()).collect();
        assert!(places.contains(&"irc.a.example #rust") && places.contains(&"irc.a.example status") && places.contains(&"irc.b.example carol"), "{places:?}");
        // the log-opened marker is never a hit
        assert!(search(&d, None, "Log opened", &Options::default()).hits.is_empty());
        // case sensitive
        let o = Options { case_sensitive: true, ..Options::default() };
        assert_eq!(search(&d, None, "HELLO", &o).hits.len(), 1);
        // one server only (matched the way the folder is named)
        assert_eq!(search(&d, Some("IRC.B.EXAMPLE"), "hello", &Options::default()).hits.len(), 1);
        assert!(search(&d, Some("nowhere"), "hello", &Options::default()).hits.is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn short_queries_and_missing_folders_find_nothing() {
        let d = temp("short");
        write(&d.join("irc.a.example").join("status.log"), "[x] a\n");
        assert_eq!(search(&d, None, "a", &Options::default()), Outcome::default());
        assert_eq!(search(&d, None, "  ", &Options::default()), Outcome::default());
        assert_eq!(search(&d.join("missing"), None, "hello", &Options::default()), Outcome::default());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_search_is_bounded() {
        let d = temp("bounded");
        let mut text = String::new();
        for i in 0..1000 {
            text.push_str(&format!("[t] <x> needle {i}\n"));
        }
        write(&d.join("irc.a.example").join("channels").join("#big.log"), &text);
        let o = Options { max_hits: 10, ..Options::default() };
        let r = search(&d, None, "needle", &o);
        assert_eq!(r.hits.len(), 10);
        assert!(r.truncated);
        // a byte limit searches the end of the file, and drops the line that may be cut in half
        let o = Options { max_file_bytes: 200, ..Options::default() };
        let r = search(&d, None, "needle", &o);
        assert!(!r.hits.is_empty() && r.hits.len() < 20, "{}", r.hits.len());
        assert!(r.hits.last().unwrap().line.ends_with("needle 999"));
        assert!(r.hits.iter().all(|h| h.line.starts_with("[t] <x> needle ")), "no cut lines");
        // an overall byte budget stops the search
        let o = Options { max_total_bytes: 0, ..Options::default() };
        assert!(search(&d, None, "needle", &o).truncated);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn odd_bytes_and_long_lines_are_handled() {
        let d = temp("odd");
        let p = d.join("irc.a.example").join("channels").join("#x.log");
        let mut f = File::create(&p).unwrap();
        f.write_all(b"[t] bad \xff\xfe bytes needle\n").unwrap();
        f.write_all(format!("[t] {} needle\n", "y".repeat(2000)).as_bytes()).unwrap();
        drop(f);
        let r = search(&d, None, "needle", &Options::default());
        assert_eq!(r.hits.len(), 2);
        assert!(r.hits[1].line.chars().count() <= 500);
        assert_eq!(hit_text(&r.hits[0]).split(']').next(), Some("[irc.a.example #x"));
        let _ = fs::remove_dir_all(&d);
    }
}
