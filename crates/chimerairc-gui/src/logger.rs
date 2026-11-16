//! Conversation logging: one plain-text file per window.
//!
//! ```text
//! <log dir>\<host>\status.log
//! <log dir>\<host>\channels\#rust.log
//! <log dir>\<host>\queries\alice.log
//! ```
//! Window names come from the network, so every path component is sanitised: no separators,
//! no `..`, no reserved Windows device names, no trailing dots. Pure std (no Win32), so it is
//! unit-tested.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    Status,
    Channel(&'a str),
    Query(&'a str),
}

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
    "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Make a name safe to use as a single Windows path component. Case-folds (the file system is
/// case-insensitive, and so is IRC), so `#Rust` and `#rust` share one log.
pub fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .flat_map(|c| c.to_lowercase())
        .take(100)
        .collect();
    // Windows silently strips trailing dots/spaces, which could alias another name.
    while s.ends_with(['.', ' ']) {
        s.pop();
        s.push('_');
    }
    // Leading dots/spaces are legal but confusing (hidden-looking, hard to select); neutralise them.
    let lead = s.chars().take_while(|c| *c == '.' || *c == ' ').count();
    s = s.chars().enumerate().map(|(i, c)| if i < lead { '_' } else { c }).collect();
    if s.is_empty() {
        return "_".into();
    }
    let stem = s.split('.').next().unwrap_or("").to_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        s.insert(0, '_');
    }
    s
}

pub fn path_for(base: &Path, host: &str, target: Target) -> PathBuf {
    let dir = base.join(sanitize(host));
    match target {
        Target::Status => dir.join("status.log"),
        Target::Channel(c) => dir.join("channels").join(format!("{}.log", sanitize(c))),
        Target::Query(n) => dir.join("queries").join(format!("{}.log", sanitize(n))),
    }
}

/// The DCC transfer log: one file for everything, `<log dir>\transfers.log`.
pub fn transfer_log_path(base: &Path) -> PathBuf {
    base.join("transfers.log")
}

/// One record of the transfer log. Everything but the numbers comes from the network or from the
/// user's own file names, so [`transfer_text`] cleans each field.
pub struct TransferEntry<'a> {
    /// Done, Failed, Cancelled, Declined or Refused.
    pub event: &'a str,
    /// Send, Receive or Chat.
    pub kind: &'a str,
    pub peer: &'a str,
    pub host: &'a str,
    pub name: &'a str,
    pub bytes: u64,
    pub size: u64,
    pub detail: &'a str,
}

/// Tab-separated fields: `event kind peer server name bytes size detail`. Tabs, newlines and other
/// control characters in a field are replaced, so a hostile nick or file name cannot forge a
/// second record or shift the columns.
pub fn transfer_text(e: &TransferEntry) -> String {
    let clean = |s: &str| -> String { s.chars().map(|c| if c.is_control() { ' ' } else { c }).take(300).collect() };
    [
        clean(e.event),
        clean(e.kind),
        clean(e.peer),
        clean(e.host),
        clean(e.name),
        e.bytes.to_string(),
        e.size.to_string(),
        clean(e.detail),
    ]
    .join("\t")
}

/// Appends timestamped lines to log files, writing a header the first time each file is touched
/// in this run so separate sessions are easy to tell apart.
#[derive(Default)]
pub struct Logger {
    opened: HashSet<PathBuf>,
}

impl Logger {
    /// Append `[stamp] text`. `stamp` is a full local date-time. Newlines in `text` are flattened.
    pub fn append(&mut self, path: &Path, stamp: &str, text: &str) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut f = OpenOptions::new().create(true).append(true).open(path)?;
        let mut out = String::new();
        if self.opened.insert(path.to_path_buf()) {
            out.push_str(&format!("--- Log opened {stamp} ---\n"));
        }
        let flat: String = text.chars().map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect();
        out.push_str(&format!("[{stamp}] {flat}\n"));
        f.write_all(out.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("chimerairc-log-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn sanitize_blocks_traversal_and_separators() {
        for evil in ["../..", "..", "a/b", "a\\b", "..\\..\\windows", "C:\\x", "x:y"] {
            let s = sanitize(evil);
            assert!(!s.contains(['/', '\\', ':']), "{evil} -> {s}");
            assert!(!s.starts_with('.'), "{evil} -> {s}");
            assert_ne!(s, "..");
        }
        assert_eq!(sanitize("#Rust"), "#rust");
        assert_eq!(sanitize("a|b*c?"), "a_b_c_");
    }

    #[test]
    fn sanitize_handles_windows_quirks() {
        assert_eq!(sanitize("con"), "_con");
        assert_eq!(sanitize("NUL.txt"), "_nul.txt");
        assert_eq!(sanitize("com1"), "_com1");
        assert_eq!(sanitize("console"), "console"); // only exact device names are reserved
        assert_eq!(sanitize("name."), "name_");
        assert!(!sanitize("name  ").ends_with([' ', '.'])); // Windows would strip these and alias "name"
        assert!(!sanitize("name. . ").ends_with([' ', '.']));
        assert_eq!(sanitize(""), "_");
        assert_eq!(sanitize("   "), "___");
        assert_eq!(sanitize("..hidden"), "__hidden");
        assert_eq!(sanitize(&"x".repeat(500)).len(), 100);
    }

    #[test]
    fn layout() {
        let base = Path::new("C:\\logs");
        assert_eq!(path_for(base, "Irc.Example.Net", Target::Status), base.join("irc.example.net").join("status.log"));
        assert_eq!(
            path_for(base, "h", Target::Channel("#Rust")),
            base.join("h").join("channels").join("#rust.log")
        );
        assert_eq!(path_for(base, "h", Target::Query("Alice")), base.join("h").join("queries").join("alice.log"));
        // a hostile channel name can never leave its directory
        let p = path_for(base, "h", Target::Channel("../../evil"));
        assert!(p.starts_with(base.join("h").join("channels")));
        // a channel and a nick that share a name do not collide
        assert_ne!(path_for(base, "h", Target::Channel("x")), path_for(base, "h", Target::Query("x")));
    }

    #[test]
    fn appends_with_one_header_per_run_and_flattens_newlines() {
        let dir = temp_dir("append");
        let path = path_for(&dir, "h", Target::Channel("#c"));
        let mut a = Logger::default();
        a.append(&path, "2026-01-02 03:04:05", "<bob> hello").unwrap();
        a.append(&path, "2026-01-02 03:04:06", "two\r\nlines").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "--- Log opened 2026-01-02 03:04:05 ---\n[2026-01-02 03:04:05] <bob> hello\n[2026-01-02 03:04:06] two  lines\n"
        );
        // a new Logger (next run) appends to the same file with a fresh header
        let mut b = Logger::default();
        b.append(&path, "2026-01-03 00:00:00", "later").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("--- Log opened 2026-01-03 00:00:00 ---\n[2026-01-03 00:00:00] later\n"));
        assert_eq!(text.matches("--- Log opened").count(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn transfer_records_are_one_line_with_fixed_columns() {
        let e = TransferEntry {
            event: "Done",
            kind: "Receive",
            peer: "bob",
            host: "irc.example.net",
            name: "my file.zip",
            bytes: 600_000,
            size: 600_000,
            detail: "",
        };
        assert_eq!(transfer_text(&e), "Done\tReceive\tbob\tirc.example.net\tmy file.zip\t600000\t600000\t");
        // hostile names cannot add columns or lines
        let evil = TransferEntry { peer: "x\ty", name: "a\r\n[2026-01-01 00:00:00] Done\tSend\tfake", detail: "line1\nline2", ..e };
        let t = transfer_text(&evil);
        assert_eq!(t.matches('\t').count(), 7, "{t:?}");
        assert!(!t.contains(['\r', '\n']));
        // absurdly long names are cut
        let long = TransferEntry { name: &"n".repeat(5000), ..evil };
        assert!(transfer_text(&long).len() < 1000);
        assert_eq!(transfer_log_path(Path::new("C:\\logs")), Path::new("C:\\logs").join("transfers.log"));
    }

    #[test]
    fn append_reports_errors_instead_of_panicking() {
        let dir = temp_dir("err");
        fs::create_dir_all(&dir).unwrap();
        // the "directory" for the log is actually a file, so create_dir_all must fail
        let blocker = dir.join("blocker");
        fs::write(&blocker, "x").unwrap();
        let path = blocker.join("status.log");
        assert!(Logger::default().append(&path, "t", "x").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
