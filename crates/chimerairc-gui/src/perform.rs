//! Auto-perform: commands that run by themselves when a connection to a network is registered (identify
//! with services, set modes, join channels, ...). Pure data and checks, unit-tested.
//!
//! A *profile* belongs to a network (its name in the server list, such as `Libera.Chat`) or to one server
//! (its host name). Its script is a list of lines, one command each, as you would type them:
//!
//! ```text
//! /msg NickServ identify {ask:NickServ password}
//! /wait 2
//! /mode {nick} +i
//! /join #rust
//! ```
//!
//! * `{nick}` and `{server}` stand for the nickname and host of the connection.
//! * `{ask:Label}` asks for a value (a password) every time the script runs; it is kept in memory only.
//! * `/wait N` pauses the script for N seconds (1 to 30), for example until services have answered.
//! * Lines starting with `;` and empty lines are comments.
//!
//! **No password is ever stored.** A line that would put one in the settings file (an `identify`, `oper`
//! or `pass` with a literal argument, a channel key) is refused; `{ask:...}` is how to log in.

/// Most lines in one profile, and most profiles.
pub const MAX_LINES: usize = 30;
pub const MAX_PROFILES: usize = 40;
/// Longest line.
pub const MAX_LINE: usize = 400;
/// Longest pause.
pub const MAX_WAIT: u32 = 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    /// A network name or a host name.
    pub network: String,
    pub lines: Vec<String>,
}

/// The settings line for one command of a profile: `network<TAB>line`.
pub fn to_setting(network: &str, line: &str) -> String {
    format!("{network}\t{line}")
}

/// Add a settings line to the profiles; invalid ones (and ones that would hold a credential) are skipped.
pub fn add_from_setting(profiles: &mut Vec<Profile>, v: &str) {
    let Some((network, line)) = v.split_once('\t') else { return };
    let (network, line) = (network.trim(), line.trim());
    if !valid_network(network) || check_line(line).is_err() {
        return;
    }
    match profiles.iter_mut().find(|p| p.network.eq_ignore_ascii_case(network)) {
        Some(p) => {
            if p.lines.len() < MAX_LINES {
                p.lines.push(line.to_owned());
            }
        }
        None => {
            if profiles.len() < MAX_PROFILES {
                profiles.push(Profile { network: network.to_owned(), lines: vec![line.to_owned()] });
            }
        }
    }
}

/// A network or host name: no spaces or control characters, and not absurdly long.
pub fn valid_network(n: &str) -> bool {
    !n.is_empty() && n.chars().count() <= 60 && !n.chars().any(|c| c.is_control() || c.is_whitespace() && c != ' ')
}

/// Replace or remove the profile of `network` (an empty script removes it). The script is checked first.
pub fn set(profiles: &mut Vec<Profile>, network: &str, script: &str) -> Result<(), String> {
    let network = network.trim();
    if !valid_network(network) {
        return Err("Enter a network name (as in the server list) or a server host name.".into());
    }
    let lines = parse_script(script)?;
    profiles.retain(|p| !p.network.eq_ignore_ascii_case(network));
    if !lines.is_empty() {
        if profiles.len() >= MAX_PROFILES {
            return Err(format!("There can be at most {MAX_PROFILES} profiles."));
        }
        profiles.push(Profile { network: network.to_owned(), lines });
    }
    Ok(())
}

/// The profile that applies to a connection to `host`: one named after the host first, then one named after
/// the network the host belongs to.
pub fn find<'a>(profiles: &'a [Profile], host: &str, network: Option<&str>) -> Option<&'a Profile> {
    profiles
        .iter()
        .find(|p| p.network.eq_ignore_ascii_case(host))
        .or_else(|| network.and_then(|n| profiles.iter().find(|p| p.network.eq_ignore_ascii_case(n))))
}

/// The lines of a script typed into the editor: blank lines and `;` comments dropped, every line checked.
pub fn parse_script(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        check_line(line).map_err(|e| format!("Line {}: {e}", n + 1))?;
        if out.len() >= MAX_LINES {
            return Err(format!("A profile can have at most {MAX_LINES} lines."));
        }
        out.push(with_implied_ask(line));
    }
    Ok(out)
}

/// `/wait N`: the number of seconds to pause.
pub fn wait_secs(line: &str) -> Option<u32> {
    let rest = line.strip_prefix('/')?;
    let (verb, arg) = rest.split_once(char::is_whitespace)?;
    (verb.eq_ignore_ascii_case("wait")).then(|| arg.trim().parse::<u32>().ok()).flatten().map(|n| n.clamp(1, MAX_WAIT))
}

/// Is this line allowed in a script?
pub fn check_line(line: &str) -> Result<(), String> {
    if line.chars().count() > MAX_LINE {
        return Err(format!("too long (at most {MAX_LINE} characters)."));
    }
    if line.chars().any(char::is_control) {
        return Err("contains a control character.".into());
    }
    let Some(cmd) = line.strip_prefix('/') else {
        return Err("every line is a command and starts with / (for example /join #rust).".into());
    };
    if cmd.starts_with('/') || cmd.trim().is_empty() {
        return Err("not a command.".into());
    }
    let verb = cmd.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if verb == "wait" {
        return match wait_secs(line) {
            Some(_) => Ok(()),
            None => Err(format!("use /wait with a number of seconds, for example /wait 2 (at most {MAX_WAIT}).")),
        };
    }
    if let Some(what) = credential_problem(cmd) {
        return Err(format!(
            "this would store {what} in the settings file, which ChimeraIRC never does. Use {{ask:Password}} instead, for example: /msg NickServ identify {{ask:NickServ password}}"
        ));
    }
    Ok(())
}

/// Tokens of a command line with every `{ask:...}` placeholder collapsed into one marker token.
fn tokens(cmd: &str) -> Vec<String> {
    let mut flat = String::new();
    let mut rest = cmd;
    while let Some(i) = rest.find("{ask:") {
        flat.push_str(&rest[..i]);
        match rest[i..].find('}') {
            Some(j) => {
                flat.push_str("{ask}");
                rest = &rest[i + j + 1..];
            }
            None => {
                flat.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    flat.push_str(rest);
    flat.split_whitespace().map(str::to_owned).collect()
}

/// Arguments that hold no secret of their own: placeholders for the nick, the server or a value asked for.
fn safe_arg(t: &str) -> bool {
    matches!(t, "{ask}" | "{nick}" | "{server}")
}

/// Words that start a services command carrying a password.
const PASSWORD_WORDS: &[&str] = &[
    "identify", "id", "login", "auth", "authenticate", "register", "ghost", "release", "recover", "regain", "drop", "group", "confirm",
    "verify", "sasl", "oper", "pass", "password",
];

/// Names that a private message to or from means "services" (and so may carry a password).
pub fn looks_like_services(target: &str) -> bool {
    let t = target.to_ascii_lowercase();
    let name = t.split('@').next().unwrap_or(&t);
    name.ends_with("serv") || matches!(name, "ns" | "cs" | "q" | "x" | "chanfix")
}

/// Is the secret among these arguments (those after the command word) given literally? It is the last
/// argument (`identify secret`, `identify account secret`, `oper name secret`), except for `register`,
/// where the password comes first and an e-mail address after it.
fn secret_is_literal(word: &str, args: &[String]) -> bool {
    let secret = if word == "register" { args.first() } else { args.last() };
    secret.is_some_and(|a| !safe_arg(a))
}

/// What kind of secret a command would carry in clear text, if any.
fn credential_problem(cmd: &str) -> Option<&'static str> {
    let toks = tokens(cmd);
    let verb = toks.first()?.to_ascii_lowercase();
    let args = &toks[1..];
    match verb.as_str() {
        // /msg NickServ identify secret
        "msg" | "privmsg" | "notice" | "query" | "squery" => {
            let target = args.first()?;
            let word = args.get(1)?.to_ascii_lowercase();
            (looks_like_services(target) && PASSWORD_WORDS.contains(&word.as_str()) && secret_is_literal(&word, &args[2..])).then_some("a password")
        }
        // /ns identify secret, /cs identify #chan secret
        "ns" | "cs" | "nickserv" | "chanserv" | "authserv" => {
            let word = args.first()?.to_ascii_lowercase();
            (PASSWORD_WORDS.contains(&word.as_str()) && secret_is_literal(&word, &args[1..])).then_some("a password")
        }
        "oper" | "pass" | "password" => secret_is_literal(&verb, args).then_some("a password"),
        // /raw PASS secret, /quote OPER name secret, /raw JOIN #chan key
        "raw" | "quote" => {
            let word = args.first()?.to_ascii_lowercase();
            if PASSWORD_WORDS.contains(&word.as_str()) && secret_is_literal(&word, &args[1..]) {
                Some("a password")
            } else if word == "join" && args.get(2).is_some_and(|a| !safe_arg(a)) {
                Some("a channel key")
            } else {
                None
            }
        }
        // /join #chan key
        "join" | "j" => args.get(1).is_some_and(|a| !safe_arg(a)).then_some("a channel key"),
        _ => None,
    }
}

/// A bare `identify` (to NickServ and friends, with nothing after it) cannot work: it needs a password. Such a
/// line gets `{ask:...}` added, so that the password is asked for when the script runs. Anything else is
/// returned as it was.
pub fn with_implied_ask(line: &str) -> String {
    let Some(cmd) = line.strip_prefix('/') else { return line.to_owned() };
    let toks = tokens(cmd);
    let verb = toks.first().map(|v| v.to_ascii_lowercase()).unwrap_or_default();
    let word_of = |i: usize| toks.get(i).map(|w| w.to_ascii_lowercase());
    let login = |w: &Option<String>| w.as_deref().is_some_and(|w| matches!(w, "identify" | "id" | "login" | "auth"));
    let (service, bare) = match verb.as_str() {
        "msg" | "privmsg" | "notice" | "squery" => (toks.get(1).cloned().unwrap_or_default(), toks.len() == 3 && login(&word_of(2)) && looks_like_services(&toks[1])),
        "ns" | "nickserv" => ("NickServ".to_owned(), toks.len() == 2 && login(&word_of(1))),
        "cs" | "chanserv" => ("ChanServ".to_owned(), toks.len() == 2 && login(&word_of(1))),
        _ => (String::new(), false),
    };
    if !bare {
        return line.to_owned();
    }
    let service = service.split('@').next().unwrap_or("").to_owned();
    let label = if service.eq_ignore_ascii_case("nickserv") || service.eq_ignore_ascii_case("ns") { "NickServ password".to_owned() } else { format!("{service} password") };
    format!("{} {{ask:{label}}}", line.trim_end())
}

/// Fill in a line: `{nick}` and `{server}` are replaced, each `{ask:Label}` is answered by `ask` (which
/// returns `None` when the user gives up, and then so does this function). The second value says whether
/// anything was asked for, so that the caller does not echo the line.
pub fn expand(line: &str, nick: &str, server: &str, ask: &mut dyn FnMut(&str) -> Option<String>) -> Option<(String, bool)> {
    let mut out = String::new();
    let mut asked = false;
    let mut rest = line;
    while let Some(i) = rest.find("{ask:") {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 5..];
        let Some(j) = tail.find('}') else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let value = ask(tail[..j].trim())?;
        // a value cannot smuggle in another line or command
        if value.chars().any(char::is_control) {
            return None;
        }
        out.push_str(&value);
        asked = true;
        rest = &tail[j + 1..];
    }
    out.push_str(rest);
    Some((out.replace("{nick}", nick).replace("{server}", server), asked))
}

/// The line as shown to the user: every `{ask:...}` becomes asterisks.
/// A message to a service as it may be shown: the command word, then `******` in place of whatever follows it
/// (`identify hunter2` -> `identify ******`), since what follows is a password or holds one.
pub fn mask_service_text(text: &str) -> String {
    let text = text.trim();
    match text.split_once(char::is_whitespace) {
        Some((verb, rest)) if !rest.trim().is_empty() => format!("{verb} ******"),
        _ => text.to_owned(),
    }
}

pub fn masked(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(i) = rest.find("{ask:") {
        out.push_str(&rest[..i]);
        match rest[i..].find('}') {
            Some(j) => {
                out.push_str("******");
                rest = &rest[i + j + 1..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_message_shows_its_verb_but_never_what_follows() {
        assert_eq!(mask_service_text("identify !Dummy1234"), "identify ******");
        assert_eq!(mask_service_text("IDENTIFY alice hunter2"), "IDENTIFY ******");
        assert_eq!(mask_service_text("  register pw me@example.org "), "register ******");
        assert_eq!(mask_service_text("identify"), "identify");
        assert_eq!(mask_service_text("identify   "), "identify");
        for secret in ["identify hunter2", "login alice hunter2", "register pw a@b.c", "auth alice pw"] {
            assert!(!mask_service_text(secret).contains("hunter2") && !mask_service_text(secret).contains(" pw"), "{secret}");
        }
    }

    #[test]
    fn ordinary_commands_and_waits_are_accepted() {
        for l in ["/join #rust", "/join #a,#b", "/mode {nick} +i", "/msg NickServ identify", "/msg #chan hello there", "/wait 2", "/nick Alice", "/raw MODE x +i", "/away gone fishing"] {
            assert_eq!(check_line(l), Ok(()), "{l}");
        }
    }

    #[test]
    fn lines_that_are_not_commands_are_refused() {
        for l in ["join #rust", "//join", "/", "/wait", "/wait x", "/wait 0x", "hello"] {
            assert!(check_line(l).is_err(), "{l}");
        }
        assert!(check_line(&format!("/msg x {}", "a".repeat(MAX_LINE))).is_err());
        assert!(check_line("/join #a\u{7}").is_err());
    }

    #[test]
    fn a_literal_password_is_never_accepted() {
        for l in [
            "/msg NickServ identify hunter2",
            "/msg nickserv IDENTIFY me hunter2",
            "/msg NickServ@services.example.net identify hunter2",
            "/notice ChanServ identify #chan hunter2",
            "/msg NS id hunter2",
            "/msg Q AUTH user hunter2",
            "/ns identify hunter2",
            "/cs identify #c hunter2",
            "/oper admin hunter2",
            "/pass hunter2",
            "/raw PASS hunter2",
            "/quote OPER a b",
            "/msg NickServ register hunter2 me@example.org",
            "/msg NickServ identify {ask:Password} hunter2",
        ] {
            assert!(check_line(l).is_err(), "{l}");
        }
        // the message explains what to do instead
        assert!(check_line("/ns identify hunter2").unwrap_err().contains("{ask:Password}"));
    }

    #[test]
    fn services_are_recognised_by_name() {
        for n in ["NickServ", "chanserv", "ChanServ@services.example.net", "HostServ", "MemoServ", "AuthServ", "NS", "Q", "X@channels.undernet.org"] {
            assert!(looks_like_services(n), "{n}");
        }
        for n in ["alice", "bob", "Serge", "#chan", "observer"] {
            assert!(!looks_like_services(n), "{n}");
        }
    }

    #[test]
    fn a_channel_key_counts_as_a_secret_too() {
        assert!(check_line("/join #secret hunter2").is_err());
        assert!(check_line("/j #secret key").is_err());
        assert!(check_line("/raw JOIN #secret key").is_err());
        assert!(check_line("/join #secret {ask:Channel key}").is_ok());
        assert!(check_line("/join #a,#b").is_ok());
    }

    #[test]
    fn asking_for_the_password_at_run_time_is_the_way_to_identify() {
        for l in [
            "/msg NickServ identify {ask:NickServ password}",
            "/msg NickServ identify {nick} {ask:Password}",
            "/ns identify {ask:Password}",
            "/oper admin {ask:Oper password}",
            "/raw PASS {ask:Server password}",
        ] {
            assert_eq!(check_line(l), Ok(()), "{l}");
        }
    }

    #[test]
    fn a_script_drops_blank_lines_and_comments_and_reports_the_bad_line() {
        let s = parse_script("; log in\n\n/msg NickServ identify {ask:Pw}\n  /wait 2  \n/join #rust\n").unwrap();
        assert_eq!(s, ["/msg NickServ identify {ask:Pw}", "/wait 2", "/join #rust"]);
        let e = parse_script("/join #a\n/pass secret\n").unwrap_err();
        assert!(e.starts_with("Line 2:"), "{e}");
        let many = (0..MAX_LINES + 1).map(|i| format!("/join #c{i}")).collect::<Vec<_>>().join("\n");
        assert!(parse_script(&many).is_err());
    }

    #[test]
    fn a_bare_identify_gets_its_question_added() {
        assert_eq!(with_implied_ask("/msg NickServ identify"), "/msg NickServ identify {ask:NickServ password}");
        assert_eq!(with_implied_ask("/msg nickserv ID"), "/msg nickserv ID {ask:NickServ password}");
        assert_eq!(with_implied_ask("/ns identify"), "/ns identify {ask:NickServ password}");
        assert_eq!(with_implied_ask("/cs identify"), "/cs identify {ask:ChanServ password}");
        assert_eq!(with_implied_ask("/msg Q AUTH"), "/msg Q AUTH {ask:Q password}");
        assert_eq!(with_implied_ask("/msg NickServ@services.example.net login"), "/msg NickServ@services.example.net login {ask:NickServ password}");
        // everything else stays as it is
        for l in ["/msg NickServ identify {ask:Pw}", "/msg NickServ info", "/msg alice identify", "/join #a", "/ns info me", "/msg NickServ identify Alice"] {
            assert_eq!(with_implied_ask(l), l, "{l}");
        }
        // and the added question is a valid line
        assert_eq!(check_line(&with_implied_ask("/msg NickServ identify")), Ok(()));
        // saving a script writes it out
        assert_eq!(parse_script("/msg NickServ identify\n/join #a").unwrap(), ["/msg NickServ identify {ask:NickServ password}", "/join #a"]);
    }

    #[test]
    fn waits_are_read_and_capped() {
        assert_eq!(wait_secs("/wait 5"), Some(5));
        assert_eq!(wait_secs("/WAIT 99"), Some(MAX_WAIT));
        assert_eq!(wait_secs("/wait 0"), Some(1));
        assert_eq!(wait_secs("/join #a"), None);
        assert_eq!(wait_secs("/wait"), None);
    }

    #[test]
    fn profiles_are_found_by_host_first_then_by_network() {
        let mut p = Vec::new();
        set(&mut p, "Libera.Chat", "/join #rust").unwrap();
        set(&mut p, "irc.eu.libera.chat", "/join #eu").unwrap();
        assert_eq!(find(&p, "irc.eu.libera.chat", Some("Libera.Chat")).unwrap().lines, ["/join #eu"]);
        assert_eq!(find(&p, "irc.libera.chat", Some("libera.chat")).unwrap().lines, ["/join #rust"], "case does not matter");
        assert!(find(&p, "irc.example.net", Some("Other")).is_none());
        assert!(find(&p, "irc.example.net", None).is_none());
    }

    #[test]
    fn setting_a_script_replaces_it_and_an_empty_one_removes_the_profile() {
        let mut p = Vec::new();
        set(&mut p, "EFnet", "/join #a").unwrap();
        set(&mut p, "efnet", "/join #b\n/join #c").unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].lines.len(), 2);
        set(&mut p, "EFnet", "; nothing left\n").unwrap();
        assert!(p.is_empty());
        assert!(set(&mut p, "", "/join #a").is_err());
        assert!(set(&mut p, "bad\tname", "/join #a").is_err());
        assert!(set(&mut p, "EFnet", "/pass x").is_err());
        assert!(p.is_empty(), "a refused script changes nothing");
    }

    #[test]
    fn settings_lines_rebuild_the_profiles_and_skip_anything_unsafe() {
        let mut p = Vec::new();
        for v in [
            "Libera.Chat\t/msg NickServ identify {ask:Pw}",
            "Libera.Chat\t/join #rust",
            "Libera.Chat\t/msg NickServ identify hunter2",
            "EFnet\t/join #a",
            "no tab here",
            "\t/join #x",
            "EFnet\tjoin #b",
        ] {
            add_from_setting(&mut p, v);
        }
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].lines, ["/msg NickServ identify {ask:Pw}", "/join #rust"], "a hand-edited password is dropped");
        assert_eq!(p[1].lines, ["/join #a"]);
        assert_eq!(to_setting("EFnet", "/join #a"), "EFnet\t/join #a");
    }

    #[test]
    fn placeholders_are_filled_in_and_the_answer_stays_out_of_the_text_shown() {
        let mut answers = vec!["hunter2".to_owned()];
        let mut ask = |label: &str| {
            assert_eq!(label, "NickServ password");
            answers.pop()
        };
        let (line, asked) = expand("/msg NickServ identify {nick} {ask:NickServ password}", "Alice", "irc.example.net", &mut ask).unwrap();
        assert_eq!(line, "/msg NickServ identify Alice hunter2");
        assert!(asked);
        assert_eq!(masked("/msg NickServ identify {nick} {ask:NickServ password}"), "/msg NickServ identify {nick} ******");
        let (plain, asked) = expand("/mode {nick} +i on {server}", "Alice", "h.example", &mut |_| None).unwrap();
        assert_eq!(plain, "/mode Alice +i on h.example");
        assert!(!asked);
    }

    #[test]
    fn giving_up_on_a_question_or_a_hostile_answer_cancels_the_line() {
        assert!(expand("/ns identify {ask:Pw}", "a", "s", &mut |_| None).is_none());
        assert!(expand("/ns identify {ask:Pw}", "a", "s", &mut |_| Some("x\r\nQUIT".into())).is_none(), "no second command through the answer");
        // an unterminated placeholder is left as it is
        assert_eq!(expand("/say {ask:oops", "a", "s", &mut |_| None).unwrap().0, "/say {ask:oops");
    }
}
