//! Read a HydraIRC installation (`HydraIRC.xml` + `Profile_*.xml`) and turn what ChimeraIRC can use
//! into ChimeraIRC settings: favorites, highlight words and the user identity.
//!
//! Pure logic apart from [`load_dir`]. Formats were taken from HydraIRC's own source
//! (`Prefs.cpp`, `FavoritesDlg.cpp`, `Channel.cpp`) and its shipped sample files.
//!
//! Deliberately NOT imported: passwords, and anything with no ChimeraIRC equivalent (buddy groups,
//! the ignore list, command profiles, themes, sounds). Those are only counted so the caller can
//! say what was left behind.

use crate::favorites::{self, Favorites, Server};
use crate::highlight::Rule;
use crate::ignorelist::{self, Entry as IgnoreEntry};
use roxmltree::{Document, Node};
use std::collections::HashMap;
use std::io;
use std::path::Path;
use chimerairc_proto::format::Rgb;

/// HydraIRC `highlightflags` bits (HydraIRCTypes.h).
const HL_SUBSTRING: u32 = 1;
const HL_REGEXP: u32 = 2;
const HL_WHOLELINE: u32 = 4;
const HL_CASESENSITIVE: u32 = 8;
/// HydraIRC notification flag: the event is switched off.
const NE_DISABLED: u32 = 2;
/// HydraIRC ignore flags (Prefs.h): disabled, and "ignore everywhere" (else only the listed channels).
const IIF_DISABLED: u32 = 1;
const IIF_EVERYWHERE: u32 = 2;
/// Notification group holding the "general" (non-buddy) events.
const GENERAL_GROUP: i32 = -2;
/// HydraIRC's default (plain) IRC port.
const PLAIN_PORT: u16 = 6667;
const TLS_PORT: u16 = 6697;
/// Refuse absurdly large files (a real HydraIRC.xml is well under 100 KB).
const MAX_FILE: u64 = 8 * 1024 * 1024;
const MAX_PROFILES: usize = 20;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub nick: String,
    pub user: String,
    pub realname: String,
}

/// Everything found in a HydraIRC folder.
#[derive(Debug, Default)]
pub struct Parsed {
    pub favorites: Favorites,
    pub highlights: Vec<Rule>,
    pub identity: Option<Identity>,
    /// Query favorites (ChimeraIRC favorites hold servers and channels only).
    pub queries_skipped: usize,
    /// Favorites with an unusable server or channel name.
    pub invalid_favorites: usize,
    /// Highlights that look like regular expressions (HydraIRC never implemented those).
    pub regex_skipped: usize,
    pub buddies: usize,
    pub ignores: Vec<IgnoreEntry>,
    /// Ignore entries that cannot work (no channels and not "everywhere": HydraIRC ignored nothing).
    pub ignores_skipped: usize,
    /// Ignore entries tied to one HydraIRC network; ChimeraIRC imports them for every server.
    pub ignores_widened: usize,
    pub command_profiles: usize,
    /// Profile names that were read.
    pub profiles: Vec<String>,
    pub found_main: bool,
    pub warnings: Vec<String>,
}

impl Parsed {
    pub fn is_empty(&self) -> bool {
        self.favorites.servers.is_empty()
            && self.highlights.is_empty()
            && self.ignores.is_empty()
            && self.identity.is_none()
    }

    pub fn channel_count(&self) -> usize {
        self.favorites.servers.iter().map(|s| s.channels.len()).sum()
    }

    /// HydraIRC predates TLS, so favorites on its standard plain port 6667 are moved to TLS on
    /// 6697. Servers that do not support it show an obvious connection error and can be edited.
    pub fn upgrade_plain_to_tls(&mut self) {
        let old = std::mem::take(&mut self.favorites.servers);
        for mut s in old {
            if s.port == PLAIN_PORT && !s.tls {
                s.port = TLS_PORT;
                s.tls = true;
            }
            merge_server(&mut self.favorites, &s);
        }
    }
}

// ---------------------------------------------------------------- reading files

pub struct LoadedFiles {
    pub main: Option<String>,
    /// (file name, contents) of every `Profile_*.xml`.
    pub profiles: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

/// HydraIRC wrote ISO-8859-1; accept UTF-8 too.
fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

fn read_limited(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_FILE {
        return Err(format!("{} is too large to be a HydraIRC file", path.display()));
    }
    std::fs::read(path).map(|b| decode(&b)).map_err(|e| format!("{}: {e}", path.display()))
}

/// Read `HydraIRC.xml` and `Profile_*.xml` from `dir` (not recursive).
pub fn load_dir(dir: &Path) -> io::Result<LoadedFiles> {
    let mut out = LoadedFiles { main: None, profiles: Vec::new(), warnings: Vec::new() };
    let mut names: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    for name in names {
        let lower = name.to_lowercase();
        let path = dir.join(&name);
        if lower == "hydrairc.xml" {
            match read_limited(&path) {
                Ok(t) => out.main = Some(t),
                Err(w) => out.warnings.push(w),
            }
        } else if lower.starts_with("profile_") && lower.ends_with(".xml") && out.profiles.len() < MAX_PROFILES {
            match read_limited(&path) {
                Ok(t) => out.profiles.push((name, t)),
                Err(w) => out.warnings.push(w),
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- parsing

fn is(n: Node, tag: &str) -> bool {
    n.is_element() && n.tag_name().name().eq_ignore_ascii_case(tag)
}

/// `network id -> name`, from the `<networklist>` in HydraIRC.xml.
fn parse_networks(doc: &Document) -> HashMap<u32, String> {
    doc.descendants()
        .filter(|n| is(*n, "network") && n.parent().is_some_and(|p| is(p, "networklist")))
        .filter_map(|n| Some((n.attribute("id")?.trim().parse().ok()?, n.attribute("name")?.trim().to_owned())))
        .collect()
}

/// HydraIRC colours are `#RRGGBB` (or the same value in decimal).
fn parse_color(s: &str) -> Option<Rgb> {
    let t = s.trim();
    let v = match t.strip_prefix('#') {
        Some(h) => u32::from_str_radix(h, 16).ok()?,
        None => t.parse::<u32>().ok()?,
    };
    Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// HydraIRC wrapped highlight text in a regular expression; plain words only are portable.
fn looks_like_regex(s: &str) -> bool {
    s.chars().any(|c| "()*+?[]{}^$\\|".contains(c))
}

/// Add `s` to `dst`, or merge its channels into the existing favorite for the same host and port.
/// Returns (server added?, channels added).
fn merge_server(dst: &mut Favorites, s: &Server) -> (bool, usize) {
    match dst.find(&s.host, s.port) {
        Some(i) => {
            let added = s.channels.iter().filter(|c| dst.servers[i].add_channel(c)).count();
            (false, added)
        }
        None => {
            let n = s.channels.len();
            dst.servers.push(s.clone());
            (true, n)
        }
    }
}

fn parse_favorite(n: Node, networks: &HashMap<u32, String>, p: &mut Parsed) {
    let kind = n.attribute("type").and_then(|t| t.trim().chars().next()).map(|c| c.to_ascii_uppercase());
    if kind == Some('Q') {
        p.queries_skipped += 1;
        return;
    }
    let host = n.attribute("servername").unwrap_or("").trim();
    let port = n.attribute("port").and_then(|v| v.trim().parse::<u16>().ok()).filter(|p| *p > 0).unwrap_or(PLAIN_PORT);
    if !favorites::valid_host(host) {
        p.invalid_favorites += 1;
        return;
    }
    let label = n
        .attribute("networkid")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .and_then(|id| networks.get(&id))
        .cloned()
        .unwrap_or_default();
    let channels: Vec<String> = match kind {
        Some('C') => vec![n.attribute("data").unwrap_or("").trim().to_owned()],
        Some('S') => Vec::new(),
        _ => {
            p.invalid_favorites += 1;
            return;
        }
    };
    if kind == Some('C') && !favorites::valid_channel(&channels[0]) {
        p.invalid_favorites += 1;
        return;
    }
    // 6697 is the standard TLS port; anything else keeps HydraIRC's plain connection.
    merge_server(&mut p.favorites, &Server::new(&label, host, port, port == TLS_PORT, &channels));
}

/// `<event name="Highlight">` entries of the general group.
fn parse_highlight(n: Node, p: &mut Parsed) {
    let flags = n.attribute("flags").and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(0);
    if flags & NE_DISABLED != 0 {
        return;
    }
    let word = n.attribute("matchstring").unwrap_or("").trim();
    let hl = n.attribute("highlightflags").and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(0);
    if word.is_empty() {
        return;
    }
    if hl & HL_REGEXP != 0 || looks_like_regex(word) {
        p.regex_skipped += 1;
        return;
    }
    let color = n.attribute("foregroundcolor").and_then(parse_color).unwrap_or(Rgb(0xc0, 0x00, 0x00));
    let rule = Rule {
        case_sensitive: hl & HL_CASESENSITIVE != 0,
        // Without the substring flag HydraIRC only matched whole words.
        whole_word: hl & HL_SUBSTRING == 0,
        whole_line: hl & HL_WHOLELINE != 0,
        ..Rule::new(word, color)
    };
    merge_highlight(&mut p.highlights, rule);
}

fn same_rule(a: &Rule, b: &Rule) -> bool {
    a.word.eq_ignore_ascii_case(&b.word)
        && a.case_sensitive == b.case_sensitive
        && a.whole_word == b.whole_word
        && a.whole_line == b.whole_line
}

/// Add a rule unless an equivalent one is already there.
fn merge_highlight(dst: &mut Vec<Rule>, r: Rule) -> bool {
    if dst.iter().any(|x| same_rule(x, &r)) {
        return false;
    }
    dst.push(r);
    true
}

/// `<ignore nick hostmask notes networkid flags><channel name=.../></ignore>`.
fn parse_ignore(n: Node, p: &mut Parsed) {
    let flags = n.attribute("flags").and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(0);
    let hostmask = n.attribute("hostmask").unwrap_or("").trim();
    let who = if hostmask.is_empty() { n.attribute("nick").unwrap_or("").trim() } else { hostmask };
    if who.is_empty() || who.contains(char::is_whitespace) {
        p.ignores_skipped += 1;
        return;
    }
    let channels: Vec<String> = n
        .children()
        .filter(|c| is(*c, "channel"))
        .filter_map(|c| c.attribute("name"))
        .map(|c| c.trim().to_owned())
        .filter(|c| favorites::valid_channel(c))
        .collect();
    let everywhere = flags & IIF_EVERYWHERE != 0;
    if !everywhere && channels.is_empty() {
        p.ignores_skipped += 1;
        return;
    }
    let mut e = IgnoreEntry::new(who);
    e.enabled = flags & IIF_DISABLED == 0;
    e.notes = n.attribute("notes").unwrap_or("").trim().to_owned();
    if !everywhere {
        e.rule.channels = channels;
    }
    if n.attribute("networkid").and_then(|v| v.trim().parse::<i64>().ok()).is_some_and(|id| id > 0) {
        p.ignores_widened += 1;
    }
    ignorelist::add(&mut p.ignores, e);
}

fn parse_identity(root: Node, p: &mut Parsed) {
    if p.identity.is_some() {
        return;
    }
    for id in root.descendants().filter(|n| is(*n, "identity")) {
        let nick = id
            .children()
            .filter(|c| is(*c, "nick"))
            .filter_map(|c| c.attribute("name"))
            .map(str::trim)
            .find(|n| !n.is_empty() && !n.contains(char::is_whitespace));
        if let Some(nick) = nick {
            let clean = |s: Option<&str>| s.unwrap_or("").trim().to_owned();
            let user = clean(id.attribute("username"));
            p.identity = Some(Identity {
                nick: nick.to_owned(),
                user: if user.is_empty() || user.contains(char::is_whitespace) { nick.to_owned() } else { user },
                realname: clean(id.attribute("realname")),
            });
            return;
        }
    }
}

fn parse_profile(text: &str, file: &str, networks: &HashMap<u32, String>, p: &mut Parsed) {
    let doc = match Document::parse(text) {
        Ok(d) => d,
        Err(e) => {
            p.warnings.push(format!("{file}: not valid XML ({e})"));
            return;
        }
    };
    let root = doc.root_element();
    p.profiles.push(root.attribute("name").map(str::to_owned).unwrap_or_else(|| file.to_owned()));

    for n in root.descendants().filter(|n| n.is_element()) {
        match n.tag_name().name().to_ascii_lowercase().as_str() {
            "favorite" => parse_favorite(n, networks, p),
            "buddy" => p.buddies += 1,
            "ignore" => parse_ignore(n, p),
            "commandprofile" => p.command_profiles += 1,
            "event" if n.attribute("name").is_some_and(|v| v.eq_ignore_ascii_case("highlight")) => {
                let in_general = n
                    .parent()
                    .filter(|g| is(*g, "group"))
                    .and_then(|g| g.attribute("id")?.trim().parse::<i32>().ok())
                    == Some(GENERAL_GROUP);
                if in_general {
                    parse_highlight(n, p);
                }
            }
            _ => {}
        }
    }
    parse_identity(root, p);
}

/// Parse already-loaded files. `main` is `HydraIRC.xml` (used for network names), `profiles` are
/// `(file name, contents)` pairs.
pub fn parse(main: Option<&str>, profiles: &[(String, String)]) -> Parsed {
    let mut p = Parsed::default();
    let main_doc = main.map(Document::parse);
    let mut networks = HashMap::new();
    match &main_doc {
        Some(Ok(d)) => {
            networks = parse_networks(d);
            p.found_main = true;
        }
        Some(Err(e)) => p.warnings.push(format!("HydraIRC.xml: not valid XML ({e})")),
        None => {}
    }
    for (name, text) in profiles {
        parse_profile(text, name, &networks, &mut p);
    }
    p
}

/// Merge `src` favorites into `dst`. Returns (servers added, channels added).
pub fn merge_favorites(dst: &mut Favorites, src: &Favorites) -> (usize, usize) {
    let (mut servers, mut channels) = (0, 0);
    for s in &src.servers {
        let (new, ch) = merge_server(dst, s);
        servers += new as usize;
        channels += ch;
    }
    (servers, channels)
}

/// Merge ignore entries, skipping ones that cover the same mask, scope and channels. Returns how many were added.
pub fn merge_ignores(dst: &mut Vec<IgnoreEntry>, src: &[IgnoreEntry]) -> usize {
    src.iter().filter(|e| ignorelist::add(dst, (*e).clone())).count()
}

/// Merge highlight rules, skipping ones already present. Returns how many were added.
pub fn merge_highlights(dst: &mut Vec<Rule>, src: &[Rule]) -> usize {
    src.iter().filter(|r| merge_highlight(dst, (*r).clone())).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: &str = r#"<?xml version="1.0" encoding="iso-8859-1" standalone="yes"?>
<HydraIRC>
  <networklist>
    <network name="EFnet" id="10362"/>
    <network name="QuakeNet" id="10271"/>
    <network name="Libera" id="1"/>
  </networklist>
  <serverlist>
    <Server name="irc.efnet.net" description="EFnet" port="6667" password="secret" network="10362"/>
  </serverlist>
</HydraIRC>"#;

    const PROFILE: &str = r##"<?xml version="1.0"?>
<HydraIRCProfile name="Default">
  <identities>
    <identity name="Me" realname="Real Name" username="myuser" id="1">
      <nick name="Alice"/>
      <nick name="Alice_"/>
    </identity>
  </identities>
  <buddygroups><group name="G" id="1"><buddy name="a"/><buddy name="b"/></group></buddygroups>
  <notifications>
    <general>
      <group id="-2">
        <event name="ChannelText" flags="10" soundid="0"/>
        <event name="Highlight" flags="64" matchstring="Hydra" foregroundcolor="#d8d800"/>
        <event name="Highlight" flags="64" matchstring="deploy" highlightflags="13" foregroundcolor="#ff0000"/>
        <event name="Highlight" flags="66" matchstring="disabled-one" foregroundcolor="#ff0000"/>
        <event name="Highlight" flags="64" matchstring="(.)*" highlightflags="4"/>
        <event name="Highlight" flags="64" matchstring="foo|bar" highlightflags="2"/>
      </group>
      <group id="100">
        <event name="Highlight" flags="64" matchstring="buddy-only" foregroundcolor="#00ff00"/>
      </group>
    </general>
  </notifications>
  <commandprofiles><commandprofile name="x" commands="/join #y"/></commandprofiles>
  <favorites>
    <group name="People" id="1">
      <favorite data="Hydra" name="Hydra on EFnet" servername="irc.efnet.net" type="Q" networkid="10362" port="6667" id="2"/>
    </group>
    <group name="Chans" id="3">
      <group name="Nested" id="4">
        <favorite data="#rust" name="#rust" servername="irc.efnet.net" type="C" networkid="10362" port="6667" id="5"/>
      </group>
      <favorite data="#Rust" name="dup" servername="IRC.EFNET.NET" type="C" networkid="10362" port="6667" id="6"/>
      <favorite data="#quake" name="q" servername="irc.quakenet.org" type="C" networkid="10271" port="6697" id="7"/>
      <favorite data="" name="empty" servername="irc.efnet.net" type="C" networkid="10362" port="6667" id="8"/>
      <favorite data="#x" name="bad host" servername="bad host" type="C" networkid="1" port="6667" id="9"/>
      <favorite data="irc.libera.chat" name="srv" servername="irc.libera.chat" type="S" networkid="1" port="6667" id="10"/>
    </group>
  </favorites>
  <ignores>
    <ignore nick="troll" hostmask="troll!*@*.evil.example" notes="spam" networkid="-1" flags="2"/>
    <ignore nick="chan-only" hostmask="chan-only!*@*" flags="0"><channel name="#noisy"/><channel name="notachannel"/></ignore>
    <ignore nick="nothing" hostmask="nothing!*@*" flags="0"/>
    <ignore nick="off" flags="3"/>
    <ignore nick="net" hostmask="net!*@*" networkid="10362" flags="2"/>
    <ignore nick="" hostmask=""/>
  </ignores>
</HydraIRCProfile>"##;

    fn parsed() -> Parsed {
        parse(Some(MAIN), &[("Profile_Default.xml".to_owned(), PROFILE.to_owned())])
    }

    #[test]
    fn favorites_are_flattened_merged_and_labelled() {
        let p = parsed();
        assert!(p.found_main);
        assert_eq!(p.profiles, ["Default"]);
        let s = &p.favorites.servers;
        assert_eq!(s.len(), 3, "{s:?}");
        // efnet: nested group flattened; "#Rust" duplicate (other case, other host case) merged away
        let efnet = &s[p.favorites.find("irc.efnet.net", 6667).unwrap()];
        assert_eq!(efnet.name, "EFnet");
        assert_eq!(efnet.channels, ["#rust"]);
        assert!(!efnet.tls);
        // 6697 is the TLS port and keeps TLS
        let quake = &s[p.favorites.find("irc.quakenet.org", 6697).unwrap()];
        assert!(quake.tls);
        assert_eq!(quake.channels, ["#quake"]);
        // S favorite becomes a server with no channels
        assert!(s[p.favorites.find("irc.libera.chat", 6667).unwrap()].channels.is_empty());
        assert_eq!(p.queries_skipped, 1);
        assert_eq!(p.invalid_favorites, 2); // empty channel + bad host
        assert_eq!(p.channel_count(), 2);
    }

    #[test]
    fn highlights_follow_hydrairc_semantics() {
        let p = parsed();
        // general group only; disabled, regex-looking and buddy-group entries are left out
        assert_eq!(p.highlights.len(), 2, "{:?}", p.highlights);
        let hydra = &p.highlights[0];
        assert_eq!(hydra.word, "Hydra");
        assert!(hydra.whole_word, "no substring flag means whole words only");
        assert!(!hydra.case_sensitive && !hydra.whole_line);
        assert_eq!(hydra.color, Rgb(0xd8, 0xd8, 0x00));
        let deploy = &p.highlights[1];
        assert_eq!(deploy.word, "deploy");
        // flags 13 = substring(1) + wholeline(4) + casesensitive(8)
        assert!(!deploy.whole_word && deploy.whole_line && deploy.case_sensitive);
        assert_eq!(p.regex_skipped, 2);
    }

    #[test]
    fn identity_and_leftovers_are_reported() {
        let p = parsed();
        assert_eq!(p.identity, Some(Identity { nick: "Alice".into(), user: "myuser".into(), realname: "Real Name".into() }));
        assert_eq!((p.buddies, p.command_profiles), (2, 1));
        assert!(!p.is_empty());
    }

    #[test]
    fn ignores_follow_hydrairc_semantics() {
        let p = parsed();
        let masks: Vec<&str> = p.ignores.iter().map(|e| e.rule.mask.as_str()).collect();
        assert_eq!(masks, ["troll!*@*.evil.example", "chan-only!*@*", "off!*@*", "net!*@*"]);
        let troll = &p.ignores[0];
        assert!(troll.enabled && troll.rule.channels.is_empty(), "flag 2 = everywhere");
        assert_eq!(troll.notes, "spam");
        // not "everywhere": only the listed (valid) channels
        assert_eq!(p.ignores[1].rule.channels, ["#noisy"]);
        // flags 3 = disabled + everywhere; nick-only entry becomes nick!*@*
        assert!(!p.ignores[2].enabled);
        assert!(p.ignores[2].rule.channels.is_empty());
        // everything is ignored (messages, notices, ctcp), as in HydraIRC
        assert!(troll.rule.messages && troll.rule.notices && troll.rule.ctcp);
        assert_eq!(p.ignores_skipped, 2, "no channels and not everywhere; empty entry");
        assert_eq!(p.ignores_widened, 1, "the network-specific one is imported for every server");
    }

    #[test]
    fn merging_ignores_does_not_duplicate() {
        let p = parsed();
        let mut dst = vec![IgnoreEntry::new("troll!*@*.evil.example")];
        assert_eq!(merge_ignores(&mut dst, &p.ignores), 3);
        assert_eq!(merge_ignores(&mut dst, &p.ignores), 0);
        assert_eq!(dst.len(), 4);
    }

    #[test]
    fn tls_upgrade_moves_plain_6667_and_merges_collisions() {
        let mut p = parsed();
        p.favorites.servers.push(Server::new("", "irc.efnet.net", 6697, true, &["#other".to_owned()]));
        p.upgrade_plain_to_tls();
        let efnet = &p.favorites.servers[p.favorites.find("irc.efnet.net", 6697).unwrap()];
        assert!(efnet.tls);
        assert_eq!(efnet.channels, ["#rust", "#other"]);
        assert!(p.favorites.find("irc.efnet.net", 6667).is_none());
        assert!(p.favorites.servers.iter().all(|s| !(s.port == 6667 && !s.tls)));
    }

    #[test]
    fn merging_into_existing_settings_does_not_duplicate() {
        let p = parsed();
        let mut fav = Favorites::default();
        fav.add_channel("irc.efnet.net", 6667, false, "#rust");
        let (servers, channels) = merge_favorites(&mut fav, &p.favorites);
        assert_eq!((servers, channels), (2, 1)); // quakenet(+1 channel), libera(0); efnet's #rust already there
        let again = merge_favorites(&mut fav, &p.favorites);
        assert_eq!(again, (0, 0));
        let mut rules = vec![p.highlights[0].clone()];
        assert_eq!(merge_highlights(&mut rules, &p.highlights), 1);
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn latin1_files_decode_and_garbage_does_not_panic() {
        // 0xE9 is "e acute" in ISO-8859-1 and invalid UTF-8
        let bytes = b"<?xml version=\"1.0\" encoding=\"iso-8859-1\"?><HydraIRCProfile name=\"Caf\xe9\"/>";
        let text = decode(bytes);
        let p = parse(None, &[("Profile_Caf.xml".to_owned(), text)]);
        assert_eq!(p.profiles, ["Caf\u{e9}"]);

        let bad = parse(Some("<<<"), &[("Profile_x.xml".to_owned(), "not xml at all".to_owned())]);
        assert!(bad.is_empty());
        assert_eq!(bad.warnings.len(), 2);
        assert!(!bad.found_main);
        let none = parse(None, &[]);
        assert!(none.is_empty() && none.warnings.is_empty());
    }

    #[test]
    fn colours_parse_hex_and_decimal() {
        assert_eq!(parse_color("#ff8000"), Some(Rgb(255, 128, 0)));
        assert_eq!(parse_color("16744448"), Some(Rgb(255, 128, 0)));
        assert_eq!(parse_color("#zz"), None);
        assert_eq!(parse_color(""), None);
    }

    /// Guard against drifting from the real format: parse the sample files HydraIRC shipped.
    #[test]
    fn parses_the_shipped_sample_files() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../hydrairc/Distribution/Release");
        if !dir.join("HydraIRC.xml").exists() {
            return; // reference source tree not present
        }
        let files = load_dir(&dir).expect("read sample dir");
        assert!(files.main.is_some());
        let p = parse(files.main.as_deref(), &files.profiles);
        assert!(p.found_main && p.warnings.is_empty(), "{:?}", p.warnings);
        assert_eq!(p.profiles, ["Default"]);
        assert!(p.channel_count() >= 5, "sample has several channel favorites");
        // the sample's Q favorite is skipped and the highlight "Hydra" comes through
        assert_eq!(p.queries_skipped, 1);
        assert!(p.highlights.iter().any(|r| r.word == "Hydra"));
        // network names resolved from HydraIRC.xml
        assert!(p.favorites.servers.iter().any(|s| s.name == "QuakeNet"));
        assert!(p.buddies > 0);
    }
}
