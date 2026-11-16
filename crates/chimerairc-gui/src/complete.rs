//! Tab completion for the input line: `/commands`, `#channels` and nicks.
//!
//! Pure logic (no Win32) so it can be unit-tested. Positions exchanged with the caller are
//! UTF-16 code-unit offsets, which is what the Win32 edit control uses.

/// Commands offered after a leading `/` (primary names, alphabetical).
pub const COMMANDS: &[&str] = &[
    "action", "alias", "away", "back", "buddy", "clear", "close", "connect", "dcc", "disconnect", "find", "help", "history", "identity",
    "ignore", "join", "leave", "list", "logsearch", "me", "mode", "monitor", "msg", "nick", "notice", "part", "perform",
    "privmsg", "query", "quit", "quote", "raw", "say", "server", "setname", "topic", "unignore", "uptime", "whois",
];

const CHANNEL_PREFIXES: [char; 4] = ['#', '&', '+', '!'];

/// What can be completed in the current window.
pub struct Context {
    pub channels: Vec<String>,
    /// Candidate nicks, most relevant first (see [`order_nicks`]).
    pub nicks: Vec<String>,
    pub my_nick: String,
    /// The names of the user's own commands (aliases), offered with the built-in ones.
    pub aliases: Vec<String>,
}

/// Recent speakers first (only those still present), then everyone else alphabetically.
pub fn order_nicks(recent: &[String], members: &[String]) -> Vec<String> {
    let lower = |s: &str| s.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for r in recent {
        if let Some(m) = members.iter().find(|m| lower(m) == lower(r)) {
            if !out.iter().any(|o| lower(o) == lower(m)) {
                out.push(m.clone());
            }
        }
    }
    let mut rest: Vec<&String> = members.iter().filter(|m| !out.iter().any(|o| lower(o) == lower(m))).collect();
    rest.sort_by_key(|m| lower(m));
    out.extend(rest.into_iter().cloned());
    out
}

fn utf16_to_byte(s: &str, idx: usize) -> usize {
    let mut units = 0;
    for (b, c) in s.char_indices() {
        if units >= idx {
            return b;
        }
        units += c.len_utf16();
    }
    s.len()
}

fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

/// An in-progress completion that Tab / Shift+Tab can cycle through.
pub struct Completion {
    /// UTF-16 offset where the replaced word starts.
    start: usize,
    /// UTF-16 length of what is currently in the line at `start` (the typed word or an inserted candidate).
    cur_len: usize,
    /// Replacement texts including any trailing separator; the typed word is appended last so
    /// cycling past the final candidate restores what was originally typed.
    items: Vec<String>,
    pos: Option<usize>,
}

impl Completion {
    /// Start a completion for the word ending at `caret` (UTF-16 offset). `None` if nothing matches.
    pub fn begin(text: &str, caret: usize, ctx: &Context) -> Option<Completion> {
        let cb = utf16_to_byte(text, caret);
        let before = &text[..cb];
        let word_start = before.rfind(char::is_whitespace).map_or(0, |i| i + before[i..].chars().next().map_or(1, |c| c.len_utf8()));
        let word = &before[word_start..];
        if word.is_empty() {
            return None;
        }
        let first_word = before[..word_start].trim().is_empty();
        let ci_prefix = |cand: &str, p: &str| cand.to_lowercase().starts_with(&p.to_lowercase());

        let mut items: Vec<String> = Vec::new();
        if first_word && word.starts_with('/') {
            let p = &word[1..];
            let mut names: Vec<&str> = COMMANDS.iter().copied().chain(ctx.aliases.iter().map(String::as_str)).filter(|c| ci_prefix(c, p)).collect();
            names.sort_by_key(|c| c.to_lowercase());
            names.dedup_by_key(|c| c.to_lowercase());
            items.extend(names.into_iter().map(|c| format!("/{c} ")));
        } else if word.starts_with(CHANNEL_PREFIXES) {
            let mut chans: Vec<&String> = ctx.channels.iter().filter(|c| ci_prefix(c, word)).collect();
            chans.sort_by_key(|c| c.to_lowercase());
            items.extend(chans.into_iter().map(|c| format!("{c} ")));
        } else {
            let (at, p) = match word.strip_prefix('@') {
                Some(rest) => ("@", rest),
                None => ("", word),
            };
            if p.is_empty() {
                return None;
            }
            let sep = if first_word && at.is_empty() { ": " } else { " " };
            items.extend(
                ctx.nicks
                    .iter()
                    .filter(|n| !n.eq_ignore_ascii_case(&ctx.my_nick) && ci_prefix(n, p))
                    .map(|n| format!("{at}{n}{sep}")),
            );
        }
        if items.is_empty() {
            return None;
        }
        items.push(word.to_owned());
        Some(Completion { start: units(&text[..word_start]), cur_len: units(word), items, pos: None })
    }

    /// Advance (or step back) and return the new line and caret (UTF-16 offset).
    pub fn step(&mut self, text: &str, back: bool) -> (String, usize) {
        let n = self.items.len();
        let next = match (self.pos, back) {
            (None, false) => 0,
            (None, true) => n - 1,
            (Some(p), false) => (p + 1) % n,
            (Some(p), true) => (p + n - 1) % n,
        };
        self.pos = Some(next);
        let repl = &self.items[next];
        let (sb, eb) = (utf16_to_byte(text, self.start), utf16_to_byte(text, self.start + self.cur_len));
        let out = format!("{}{}{}", &text[..sb], repl, &text[eb..]);
        self.cur_len = units(repl);
        (out, self.start + self.cur_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            channels: vec!["#Rust".into(), "#rustaceans".into(), "#other".into()],
            nicks: vec!["alice".into(), "Alex".into(), "bob".into(), "me".into()],
            my_nick: "me".into(),
            aliases: vec!["slap".into(), "hi".into()],
        }
    }

    fn run(text: &str, presses: usize) -> String {
        let mut cur = text.to_owned();
        let caret = units(text);
        let mut c = match Completion::begin(&cur, caret, &ctx()) {
            Some(c) => c,
            None => return cur,
        };
        for _ in 0..presses {
            cur = c.step(&cur, false).0;
        }
        cur
    }

    #[test]
    fn nick_at_line_start_gets_colon() {
        assert_eq!(run("al", 1), "alice: ");
        assert_eq!(run("al", 2), "Alex: ");
        assert_eq!(run("al", 3), "al"); // wraps through the original text
        assert_eq!(run("al", 4), "alice: ");
    }

    #[test]
    fn nick_mid_line_gets_space_and_keeps_prefix_text() {
        assert_eq!(run("hello bo", 1), "hello bob ");
        assert_eq!(run("hey @al", 1), "hey @alice ");
    }

    #[test]
    fn never_completes_own_nick_or_empty_word() {
        assert_eq!(run("m", 1), "m");
        assert!(Completion::begin("hello ", 6, &ctx()).is_none());
        assert!(Completion::begin("@", 1, &ctx()).is_none());
    }

    #[test]
    fn commands_only_as_first_word() {
        assert_eq!(run("/jo", 1), "/join ");
        assert_eq!(run("/p", 1), "/part ");
        assert_eq!(run("/p", 2), "/perform ");
        assert_eq!(run("/p", 3), "/privmsg ");
        // your own commands are offered too, in order with the built-in ones
        assert_eq!(run("/sl", 1), "/slap ");
        assert_eq!(run("/h", 1), "/help ");
        assert_eq!(run("/h", 2), "/hi ");
        assert_eq!(run("/h", 3), "/history ");
        assert_eq!(run("say /jo", 1), "say /jo"); // not first word, no nick matches "/jo"
    }

    #[test]
    fn channels_sorted_case_insensitive() {
        assert_eq!(run("join #ru", 1), "join #Rust ");
        assert_eq!(run("join #ru", 2), "join #rustaceans ");
    }

    #[test]
    fn backwards_cycling_and_midline_caret() {
        let text = "al and more";
        let mut c = Completion::begin(text, 2, &ctx()).unwrap(); // caret after "al"
        let (t, caret) = c.step(text, true); // Shift+Tab: starts at the original word
        assert_eq!((t.as_str(), caret), ("al and more", 2));
        let (t, caret) = c.step(&t, true);
        assert_eq!(t, "Alex:  and more");
        assert_eq!(caret, 6);
    }

    #[test]
    fn utf16_offsets_with_astral_chars() {
        let text = "😀 al"; // emoji is 2 UTF-16 units
        let caret = units(text);
        let mut c = Completion::begin(text, caret, &ctx()).unwrap();
        assert_eq!(c.step(text, false).0, "😀 alice ");
    }

    #[test]
    fn ordering_prefers_recent_speakers() {
        let members: Vec<String> = ["zed", "Amy", "bob"].map(String::from).to_vec();
        let recent: Vec<String> = ["bob", "gone", "ZED", "bob"].map(String::from).to_vec();
        assert_eq!(order_nicks(&recent, &members), ["bob", "zed", "Amy"]);
        assert_eq!(order_nicks(&[], &members), ["Amy", "bob", "zed"]);
    }
}
