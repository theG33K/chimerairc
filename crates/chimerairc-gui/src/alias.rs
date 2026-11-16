//! Aliases: your own commands. `/alias slap /me slaps $1 around a bit with a large trout` makes `/slap bob`.
//! Pure data and text expansion, unit-tested.
//!
//! An alias has a name and a text of one or more commands separated by ` | `. When it runs, each command is
//! typed as if you had entered it (a line without a leading `/` is said in the window). In the text:
//!
//! * `$1` .. `$9` the words you typed after the alias, `$1-` word 1 and all after it, `$*` everything typed
//! * `$nick` your nickname, `$chan` the channel or person of the window, `$server` its server, `$uptime` how long
//!   the program has been running, `$$` a dollar sign
//!
//! A built-in command is never replaced by an alias. Like auto-perform, an alias cannot hold a password: a command
//! that would is refused (see `perform::check_line`).

pub const MAX_ALIASES: usize = 100;
pub const MAX_COMMANDS: usize = 10;
pub const MAX_TEXT: usize = 400;
pub const MAX_NAME: usize = 20;
/// How deep aliases may call aliases.
pub const MAX_DEPTH: u32 = 4;

/// The commands the program has itself; an alias cannot take their names.
pub const BUILTINS: &[&str] = &[
    "help", "?", "server", "connect", "ignore", "unignore", "perform", "autoperform", "buddy", "buddies", "monitor", "find",
    "logsearch", "identity", "setname", "importhydra", "list", "join", "j", "part", "leave", "msg", "privmsg", "notice",
    "query", "q", "dcc", "me", "action", "mode", "away", "whois", "wi", "ns", "nickserv", "cs", "chanserv", "nick", "history",
    "topic", "quit", "disconnect", "raw", "quote", "clear", "close", "alias", "uptime", "say", "back", "stoppaste",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    /// Lower case, without the slash.
    pub name: String,
    /// The commands, separated by ` | `.
    pub text: String,
}

impl Alias {
    /// The commands of the text, in order.
    pub fn commands(&self) -> Vec<String> {
        commands(&self.text)
    }
}

pub fn commands(text: &str) -> Vec<String> {
    text.split(" | ").map(str::trim).filter(|c| !c.is_empty()).map(str::to_owned).collect()
}

/// A name that can be typed after a slash: letters, digits, `_` and `-`, starting with a letter.
pub fn valid_name(n: &str) -> bool {
    n.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && n.chars().count() <= MAX_NAME
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn is_builtin(name: &str) -> bool {
    BUILTINS.iter().any(|b| b.eq_ignore_ascii_case(name))
}

/// Check a name and a text and make an alias of them.
pub fn build(name: &str, text: &str) -> Result<Alias, String> {
    let name = name.trim().trim_start_matches('/').to_ascii_lowercase();
    if !valid_name(&name) {
        return Err(format!("\"{name}\" is not a usable alias name (letters, digits, _ and -, starting with a letter, up to {MAX_NAME})."));
    }
    if is_builtin(&name) {
        return Err(format!("/{name} is a built-in command and cannot be an alias."));
    }
    let text = text.trim();
    if text.chars().any(|c| c.is_control()) {
        return Err("The text of an alias is one line without control characters.".into());
    }
    if text.chars().count() > MAX_TEXT {
        return Err(format!("The text of an alias is at most {MAX_TEXT} characters."));
    }
    let cmds = commands(text);
    if cmds.is_empty() {
        return Err(format!("/{name} has no text."));
    }
    if cmds.len() > MAX_COMMANDS {
        return Err(format!("An alias holds at most {MAX_COMMANDS} commands."));
    }
    for c in &cmds {
        // a line that would put a password in the settings file (the same rule as auto-perform)
        crate::perform::check_line(c)?;
    }
    Ok(Alias { name, text: cmds.join(" | ") })
}

/// Add or replace an alias.
pub fn set(list: &mut Vec<Alias>, a: Alias) -> Result<(), String> {
    list.retain(|x| x.name != a.name);
    if list.len() >= MAX_ALIASES {
        return Err(format!("There can be at most {MAX_ALIASES} aliases."));
    }
    list.push(a);
    list.sort_by(|x, y| x.name.cmp(&y.name));
    Ok(())
}

pub fn remove(list: &mut Vec<Alias>, name: &str) -> bool {
    let name = name.trim().trim_start_matches('/').to_ascii_lowercase();
    let before = list.len();
    list.retain(|x| x.name != name);
    list.len() != before
}

pub fn find<'a>(list: &'a [Alias], name: &str) -> Option<&'a Alias> {
    list.iter().find(|a| a.name.eq_ignore_ascii_case(name))
}

/// `name<TAB>text`.
pub fn to_setting(a: &Alias) -> String {
    format!("{}\t{}", a.name, a.text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>())
}

pub fn from_setting(v: &str) -> Option<Alias> {
    let (name, text) = v.split_once('\t')?;
    build(name, text).ok()
}

/// The lines of the editor: `name text`, `/name text` or `name = text`, one alias per line; blank lines and lines
/// starting with `;` are skipped. Errors say which line.
pub fn parse_editor(text: &str) -> Result<Vec<Alias>, String> {
    let mut out: Vec<Alias> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        let split = line.find(|c: char| c.is_whitespace() || c == '=' || c == ':').unwrap_or(line.len());
        let (name, rest) = line.split_at(split);
        let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=' || c == ':');
        let a = build(name, rest).map_err(|e| format!("Line {}: {e}", i + 1))?;
        set(&mut out, a).map_err(|e| format!("Line {}: {e}", i + 1))?;
    }
    Ok(out)
}

/// The aliases as the editor shows them.
pub fn editor_text(list: &[Alias]) -> String {
    list.iter().map(|a| format!("{} = {}", a.name, a.text)).collect::<Vec<_>>().join("\r\n")
}

/// What the `$` words of an alias stand for besides the arguments.
pub struct Vars<'a> {
    pub nick: &'a str,
    pub chan: &'a str,
    pub server: &'a str,
    pub uptime: &'a str,
}

/// Fill in the `$` words of one command of an alias.
pub fn expand(text: &str, args: &str, vars: &Vars) -> String {
    let words: Vec<&str> = args.split_whitespace().collect();
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let rest: String = chars[i + 1..].iter().take(8).collect();
        if rest.starts_with('$') {
            out.push('$');
            i += 2;
        } else if rest.starts_with('*') {
            out.push_str(args.trim());
            i += 2;
        } else if let Some(d) = rest.chars().next().filter(|c| ('1'..='9').contains(c)) {
            let n = d as usize - '0' as usize;
            if rest.chars().nth(1) == Some('-') {
                out.push_str(&words.iter().skip(n - 1).copied().collect::<Vec<_>>().join(" "));
                i += 3;
            } else {
                out.push_str(words.get(n - 1).copied().unwrap_or(""));
                i += 2;
            }
        } else {
            let lower = rest.to_ascii_lowercase();
            let named: [(&str, &str); 5] = [("nick", vars.nick), ("me", vars.nick), ("chan", vars.chan), ("server", vars.server), ("uptime", vars.uptime)];
            match named.iter().find(|(n, _)| lower.starts_with(n) && !lower[n.len()..].starts_with(|c: char| c.is_ascii_alphanumeric())) {
                Some((n, v)) => {
                    out.push_str(v);
                    i += 1 + n.len();
                }
                None => {
                    out.push('$');
                    i += 1;
                }
            }
        }
    }
    out
}

/// `3d 4h 5m`, `2h 3m`, `5m 20s`, `42s`: the two biggest units that are not zero.
pub fn duration_text(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60, secs % 60);
    let parts: Vec<String> = [(d, "d"), (h, "h"), (m, "m"), (s, "s")].iter().filter(|(v, _)| *v > 0).map(|(v, u)| format!("{v}{u}")).take(3).collect();
    if parts.is_empty() {
        "0s".into()
    } else if d > 0 || h > 0 {
        parts.iter().take(3).cloned().collect::<Vec<_>>().join(" ")
    } else {
        parts.iter().take(2).cloned().collect::<Vec<_>>().join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars<'static> {
        Vars { nick: "me", chan: "#rust", server: "irc.example.net", uptime: "2h 3m" }
    }

    #[test]
    fn names_and_texts_are_checked() {
        assert!(build("slap", "/me slaps $1").is_ok());
        assert_eq!(build("/Slap", "/me x").unwrap().name, "slap");
        for bad in ["", "1abc", "a b", "a.b", "-x", &"x".repeat(21)] {
            assert!(build(bad, "/me x").is_err(), "{bad:?}");
        }
        assert!(build("join", "/me x").is_err(), "a built-in is not replaced");
        assert!(build("JOIN", "/me x").is_err());
        assert!(build("empty", "   ").is_err());
        assert!(build("empty", " | ").is_err());
        assert!(build("long", &"a".repeat(401)).is_err());
        assert!(build("ctl", "/me a\u{7}b").is_err());
        assert!(build("many", &vec!["/me x"; 11].join(" | ")).is_err());
        assert!(build("ten", &vec!["/me x"; 10].join(" | ")).is_ok());
    }

    #[test]
    fn an_alias_cannot_hold_a_password() {
        assert!(build("id", "/msg NickServ identify hunter2").is_err());
        assert!(build("id", "/me waves | /oper admin secret").is_err());
        assert!(build("ok", "/msg NickServ help").is_ok());
    }

    #[test]
    fn dollar_words_are_filled_in() {
        let v = vars();
        assert_eq!(expand("/me slaps $1 around with $2", "bob a trout", &v), "/me slaps bob around with a");
        assert_eq!(expand("/say $1-", "a b c", &v), "/say a b c");
        assert_eq!(expand("/say $2-", "a b c", &v), "/say b c");
        assert_eq!(expand("/say $*", "  a  b ", &v), "/say a  b");
        assert_eq!(expand("hello $1!", "", &v), "hello !", "a missing word is empty");
        assert_eq!(expand("$nick in $chan on $server up $uptime", "", &v), "me in #rust on irc.example.net up 2h 3m");
        assert_eq!(expand("$NICK $me", "", &v), "me me");
        assert_eq!(expand("costs $$5 and $unknown and $nickname", "", &v), "costs $5 and $unknown and $nickname");
        assert_eq!(expand("trailing $", "", &v), "trailing $");
        assert_eq!(expand("$10", "a", &v), "a0", "only $1 to $9 exist");
    }

    #[test]
    fn the_editor_text_round_trips_and_reports_the_bad_line() {
        let list = parse_editor("slap = /me slaps $1 | /say ouch\n; comment\n\n/hi: /say hello $1\nbye /quit gone").unwrap();
        assert_eq!(list.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["bye", "hi", "slap"]);
        assert_eq!(find(&list, "SLAP").unwrap().commands(), ["/me slaps $1", "/say ouch"]);
        assert_eq!(parse_editor(&editor_text(&list).replace("\r\n", "\n")).unwrap(), list);
        let e = parse_editor("ok /me x\n1bad /me y").unwrap_err();
        assert!(e.starts_with("Line 2:"), "{e}");
        let e = parse_editor("join /me x").unwrap_err();
        assert!(e.contains("built-in"), "{e}");
        assert_eq!(parse_editor("").unwrap(), vec![]);
    }

    #[test]
    fn the_list_is_bounded_and_settings_lines_round_trip() {
        let mut list = Vec::new();
        for i in 0..MAX_ALIASES {
            set(&mut list, build(&format!("a{i}"), "/me x").unwrap()).unwrap();
        }
        assert!(set(&mut list, build("one-too-many", "/me x").unwrap()).is_err());
        assert!(set(&mut list, build("a0", "/me y").unwrap()).is_ok(), "replacing is fine");
        assert!(remove(&mut list, "/A0") && !remove(&mut list, "a0"));
        let a = build("slap", "/me slaps $1 | /say ok").unwrap();
        assert_eq!(from_setting(&to_setting(&a)), Some(a));
        for junk in ["", "slap", "join\t/me x", "1x\t/me x", "x\t", "x\t/msg NickServ identify pw"] {
            assert!(from_setting(junk).is_none(), "{junk:?}");
        }
    }

    #[test]
    fn durations_show_the_biggest_units() {
        assert_eq!(duration_text(0), "0s");
        assert_eq!(duration_text(42), "42s");
        assert_eq!(duration_text(320), "5m 20s");
        assert_eq!(duration_text(7380), "2h 3m");
        assert_eq!(duration_text(3 * 86_400 + 4 * 3600 + 5 * 60 + 6), "3d 4h 5m");
    }
}
