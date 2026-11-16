//! Auto-reject rules for incoming DCC offers: offers that match are dropped without a prompt
//! (and logged), so a flood of unwanted files or chat requests never reaches the screen.
//!
//! Rules only ever *reject*; nothing here accepts anything, so a mistake in them can make the
//! client quieter but never less safe. Pure std, unit-tested.

use chimerairc_proto::dcc::is_risky_extension;

#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct Rules {
    /// Lower-case extensions without the dot; the word `risky` stands for every program/script type.
    pub exts: Vec<String>,
    /// Reject files larger than this; 0 = no limit.
    pub max_bytes: u64,
    pub reject_chat: bool,
}

/// Turn user input like `exe, .SCR;bat  risky` into a clean, de-duplicated list. Anything that
/// could not be an extension (path separators, wildcards, ...) is dropped.
pub fn parse_exts(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in s.split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
        let t = t.trim().trim_start_matches('.').to_lowercase();
        if t.is_empty() || t.len() > 16 || !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            continue;
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.trim_end_matches([' ', '.']).rsplit_once('.')?;
    (!stem.is_empty() || !ext.is_empty()).then(|| ext.to_lowercase())
}

impl Rules {
    pub fn new(exts: &str, max_mb: u64, reject_chat: bool) -> Rules {
        Rules { exts: parse_exts(exts), max_bytes: max_mb.saturating_mul(1024 * 1024), reject_chat }
    }

    /// Why this file offer must be dropped, if it must. Both the name the sender gave and the name
    /// we would save it under are checked, so renaming tricks do not slip past.
    pub fn file(&self, offered: &str, saved: &str, size: u64) -> Option<String> {
        if self.max_bytes > 0 && size > self.max_bytes {
            return Some(format!("the file is larger than your limit of {} MB", self.max_bytes / (1024 * 1024)));
        }
        for name in [offered, saved] {
            if self.exts.iter().any(|e| e == "risky") && is_risky_extension(name) {
                return Some("it is a program or script type and you reject those".to_owned());
            }
            if let Some(ext) = extension(name) {
                if self.exts.iter().any(|e| *e == ext) {
                    return Some(format!("you reject .{ext} files"));
                }
            }
        }
        None
    }

    pub fn chat(&self) -> Option<&'static str> {
        self.reject_chat.then_some("you reject DCC chat requests")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_lists_are_cleaned() {
        assert_eq!(parse_exts("exe, .SCR;bat  risky exe"), ["exe", "scr", "bat", "risky"]);
        assert_eq!(parse_exts("*.exe a/b ..\\x ok"), ["ok"], "wildcards and separators are not extensions");
        assert_eq!(parse_exts("  ,; "), Vec::<String>::new());
        assert!(parse_exts(&"x".repeat(40)).is_empty());
    }

    #[test]
    fn rejects_by_extension_case_insensitively_on_either_name() {
        let r = Rules::new("exe,scr", 0, false);
        assert!(r.file("Setup.EXE", "setup.exe", 10).is_some());
        assert!(r.file("fine.txt", "fine.txt", 10).is_none());
        // the sender's name is checked as well as the one we would save
        assert!(r.file("evil.scr", "evil_.txt", 10).is_some());
        // only the real (last) extension counts
        assert!(r.file("exe.txt", "exe.txt", 10).is_none());
        assert!(r.file("noextension", "noextension", 10).is_none());
        assert!(r.file("trailing.exe. ", "trailing.exe", 10).is_some(), "trailing dots/spaces are stripped by Windows");
    }

    #[test]
    fn risky_keyword_covers_the_program_types() {
        let r = Rules::new("risky", 0, false);
        for n in ["a.exe", "a.BAT", "a.ps1", "a.js", "a.iso"] {
            assert!(r.file(n, n, 1).is_some(), "{n}");
        }
        assert!(r.file("a.txt", "a.txt", 1).is_none());
        assert!(r.file("a.zip", "a.zip", 1).is_none());
    }

    #[test]
    fn size_limit_and_chat_switch() {
        let r = Rules::new("", 5, true);
        assert!(r.file("a.bin", "a.bin", 5 * 1024 * 1024).is_none(), "exactly at the limit is fine");
        assert!(r.file("a.bin", "a.bin", 5 * 1024 * 1024 + 1).unwrap().contains("5 MB"));
        assert!(r.chat().is_some());
        let none = Rules::new("", 0, false);
        assert!(none.file("a.bin", "a.bin", u64::MAX).is_none(), "0 means no limit");
        assert!(none.chat().is_none());
        assert_eq!(Rules::default(), none);
        // an absurd limit does not overflow
        assert!(Rules::new("", u64::MAX, false).max_bytes > 0);
    }
}
