//! Persistent user settings (`%APPDATA%\ChimeraIRC\settings.ini`, simple `key=value` lines).
//! Passwords are deliberately never stored.

use crate::buddies::{self, Buddy};
use crate::columns::{self, Widths};
use crate::winsizes::{self, Saved as ChildWindow};
use crate::favorites::{Favorites, Server as FavServer};
use crate::layouts::{Edge, Layout, SLOTS};
use crate::servers::Server as ListedServer;
use crate::alias::{self, Alias};
use crate::identity::{self, Identity};
use crate::perform::{self, Profile as PerformProfile};
use crate::stspolicy::Policy as StsPolicy;
use crate::theme::{self, Choice, FontSpec, Overrides};
use crate::winplace::Placement;
use crate::highlight::Rule;
use crate::ignorelist::Entry as IgnoreEntry;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub nick: String,
    pub user: String,
    pub realname: String,
    pub quit_msg: String,
    pub timestamps: bool,
    pub show_toolbar: bool,
    pub show_userlist: bool,
    /// Highlight lines that mention our own nick.
    pub hl_nick: bool,
    /// Flash the taskbar button on a highlight while the app is in the background.
    pub hl_flash: bool,
    /// Play the system sound on a highlight while the app is in the background.
    pub hl_beep: bool,
    pub highlights: Vec<Rule>,
    /// Write every window's output to a log file. Off by default: logs are private data.
    pub logging: bool,
    /// Log folder; empty means `%APPDATA%\ChimeraIRC\logs`.
    pub log_dir: String,
    pub favorites: Favorites,
    /// Servers the user added to the server list (the built-in ones are not stored).
    pub servers: Vec<ListedServer>,
    /// The docks: which edges have one, their panes, sizes and front panes.
    pub dock: Layout,
    /// Where the main window was last closed.
    pub window: Option<Placement>,
    /// Column widths of the list panes and windows.
    pub columns: Vec<Widths>,
    /// Where chat windows were when they were closed.
    pub child_windows: Vec<ChildWindow>,
    /// The five saved dock layouts.
    pub layouts: [Option<Layout>; SLOTS],
    pub ignores: Vec<IgnoreEntry>,
    /// People whose coming and going is shown.
    pub buddies: Vec<Buddy>,
    /// Light, dark, or follow Windows.
    pub theme: Choice,
    /// Chat colours chosen by the user, on top of the theme.
    pub colors: Overrides,
    /// The font of chat text and of the entry boxes, and the font of the rest of the interface.
    pub chat_font: FontSpec,
    pub ui_font: FontSpec,
    /// Auto-perform scripts, one per network or server (see `perform`).
    pub perform: Vec<PerformProfile>,
    /// Nickname, user name, real name and SASL account per network or server (see `identity`).
    pub identities: Vec<Identity>,
    /// Your own commands (see `alias`).
    pub aliases: Vec<Alias>,
    /// What was typed in the filter box of each list pane (pane name, text); empty ones are not kept.
    pub filters: Vec<(String, String)>,
    /// Buddy groups that are folded up in the Buddies pane (group keys, see `buddies::group_key`).
    pub collapsed_groups: Vec<String>,
    /// Public IP to put in DCC offers (empty = the address of the IRC connection).
    pub dcc_ip: String,
    /// Port range to listen on for DCC, e.g. `5000-5100` (empty = any free port).
    pub dcc_ports: String,
    /// Auto-reject incoming DCC files with these extensions (comma list; `risky` = all program types).
    pub dcc_reject_exts: String,
    /// Auto-reject incoming DCC files larger than this many MB (0 = no limit).
    pub dcc_max_mb: u64,
    /// Auto-reject every incoming DCC chat request.
    pub dcc_reject_chat: bool,
    /// Speed limit for each DCC file transfer in KB/s (0 = unlimited).
    pub dcc_limit_kb: u64,
    /// Ask the router (UPnP) to forward the DCC port when we are behind one. Off by default.
    pub dcc_upnp: bool,
    /// Lines of chat history to ask for when joining a channel (0 = none); needs `chathistory`.
    pub history_on_join: u32,
    /// Show who is typing to us (the `+typing` tag), and tell others when we type.
    pub typing_show: bool,
    pub typing_send: bool,
    /// Look for a newer release on GitHub when the program starts (at most once a day). Off unless switched on:
    /// nothing is sent anywhere until the user asks.
    pub update_check: bool,
    /// When the last automatic check was made (seconds since 1970).
    pub update_last: u64,
    /// Remembered Strict Transport Security policies.
    pub sts: Vec<StsPolicy>,
    /// PEM file with the TLS client certificate (and its key, unless `client_key` names another file).
    pub client_cert: String,
    pub client_key: String,
    pub last_server: String,
    pub last_port: String,
    pub last_tls: bool,
    pub last_sasl_account: String,
    pub last_autojoin: String,
}

impl Default for Settings {
    fn default() -> Self {
        let nick = default_nick();
        Settings {
            user: nick.clone(),
            nick,
            realname: "ChimeraIRC".into(),
            quit_msg: "ChimeraIRC".into(),
            timestamps: true,
            show_toolbar: true,
            show_userlist: true,
            hl_nick: true,
            hl_flash: true,
            hl_beep: false,
            highlights: Vec::new(),
            logging: false,
            log_dir: String::new(),
            favorites: Favorites::default(),
            servers: Vec::new(),
            dock: Layout::default(),
            window: None,
            columns: Vec::new(),
            child_windows: Vec::new(),
            layouts: [None; SLOTS],
            ignores: Vec::new(),
            buddies: Vec::new(),
            collapsed_groups: Vec::new(),
            filters: Vec::new(),
            perform: Vec::new(),
            identities: Vec::new(),
            aliases: Vec::new(),
            theme: Choice::Light,
            colors: Overrides::default(),
            chat_font: FontSpec::chat_default(),
            ui_font: FontSpec::ui_default(),
            dcc_ip: String::new(),
            dcc_ports: String::new(),
            dcc_reject_exts: String::new(),
            dcc_max_mb: 0,
            dcc_reject_chat: false,
            dcc_limit_kb: 0,
            dcc_upnp: false,
            history_on_join: 50,
            typing_show: true,
            typing_send: true,
            update_check: false,
            update_last: 0,
            sts: Vec::new(),
            client_cert: String::new(),
            client_key: String::new(),
            last_server: "irc.libera.chat".into(),
            last_port: "6697".into(),
            last_tls: true,
            last_sasl_account: String::new(),
            last_autojoin: String::new(),
        }
    }
}

fn default_nick() -> String {
    let u: String = std::env::var("USERNAME")
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(9)
        .collect();
    if u.is_empty() || u.starts_with(|c: char| c.is_ascii_digit()) {
        "chimerairc".into()
    } else {
        u
    }
}

fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_owned()
}

impl Settings {
    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        let mut dock_lines: Vec<String> = Vec::new();
        // the single dock of older files: visible, width, panes, front pane, edge
        let mut legacy = (true, 220u32, crate::layouts::ALL_PANES, 0u8, Edge::Left);
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            let b = v == "1";
            match k.trim() {
                "nick" if !v.is_empty() => s.nick = v.into(),
                "user" if !v.is_empty() => s.user = v.into(),
                "realname" if !v.is_empty() => s.realname = v.into(),
                "quit_msg" => s.quit_msg = v.into(),
                "timestamps" => s.timestamps = b,
                "show_toolbar" => s.show_toolbar = b,
                "show_userlist" => s.show_userlist = b,
                "hl_nick" => s.hl_nick = b,
                "hl_flash" => s.hl_flash = b,
                "hl_beep" => s.hl_beep = b,
                "logging" => s.logging = b,
                "log_dir" => s.log_dir = v.into(),
                "dcc_ip" => s.dcc_ip = v.into(),
                "dcc_ports" => s.dcc_ports = v.into(),
                "dcc_reject_exts" => s.dcc_reject_exts = v.into(),
                "dcc_max_mb" => s.dcc_max_mb = v.parse().unwrap_or(0),
                "dcc_reject_chat" => s.dcc_reject_chat = b,
                "dcc_limit_kb" => s.dcc_limit_kb = v.parse().unwrap_or(0),
                "dcc_upnp" => s.dcc_upnp = b,
                "sts" => s.sts.extend(StsPolicy::from_setting(v)),
                "client_cert" => s.client_cert = v.into(),
                "client_key" => s.client_key = v.into(),
                "typing_show" => s.typing_show = b,
                "typing_send" => s.typing_send = b,
                "update_check" => s.update_check = b,
                "update_last" => s.update_last = v.parse().unwrap_or(0),
                "history_lines" => s.history_on_join = v.parse::<u32>().unwrap_or(50).min(500),
                "server" => s.servers.extend(ListedServer::from_setting(v)),
                "dock" => dock_lines.push(v.to_owned()),
                "dock_visible" => legacy.0 = b,
                "dock_width" => legacy.1 = v.parse::<u32>().unwrap_or(220),
                "dock_pane" => legacy.3 = v.parse::<u8>().unwrap_or(0),
                "dock_edge" => legacy.4 = v.parse::<u8>().ok().and_then(Edge::from_index).unwrap_or_default(),
                "dock_panes" => legacy.2 = v.parse::<crate::layouts::Mask>().unwrap_or(crate::layouts::ALL_PANES),
                "window" => s.window = Placement::from_setting(v),
                "cols" => {
                    if let Some(w) = Widths::from_setting(v) {
                        columns::set(&mut s.columns, &w.key, &w.widths);
                    }
                }
                "child" => {
                    if let Some(c) = ChildWindow::from_setting(v) {
                        winsizes::remember(&mut s.child_windows, c);
                    }
                }
                "layout" => {
                    if let Some((slot, l)) = Layout::from_setting(v) {
                        s.layouts[slot] = Some(l);
                    }
                }
                "ignore" => s.ignores.extend(IgnoreEntry::from_setting(v)),
                "theme" => s.theme = Choice::parse(v).unwrap_or_default(),
                "chat_font" => s.chat_font = FontSpec::from_setting(v).unwrap_or_else(FontSpec::chat_default),
                "ui_font" => s.ui_font = FontSpec::from_setting(v).unwrap_or_else(FontSpec::ui_default),
                "color" => {
                    if let Some((name, c)) = v.split_once('\t') {
                        if let Some(c) = theme::parse_hex(c) {
                            s.colors.set(name.trim(), Some(c));
                        }
                    }
                }
                "perform" => perform::add_from_setting(&mut s.perform, v),
                "alias" => {
                    if let Some(a) = alias::from_setting(v) {
                        let _ = alias::set(&mut s.aliases, a);
                    }
                }
                "identity" => {
                    // the value has tabs, which `trim` above leaves alone inside; only the ends were trimmed
                    if let Some(i) = identity::from_setting(v).filter(|_| s.identities.len() < identity::MAX_IDENTITIES) {
                        s.identities.retain(|x| !x.network.eq_ignore_ascii_case(&i.network));
                        s.identities.push(i);
                    }
                }
                "filter" => {
                    if let Some((k, t)) = v.split_once('\t') {
                        let (k, t) = (k.trim(), t.trim());
                        if !k.is_empty() && !t.is_empty() && k.len() <= 20 && t.chars().count() <= 100 && s.filters.len() < 12 && !s.filters.iter().any(|(x, _)| x == k) {
                            s.filters.push((k.to_owned(), t.to_owned()));
                        }
                    }
                }
                "buddy_collapsed" => {
                    let k = v.trim();
                    if !k.is_empty() && k.len() <= 60 && !s.collapsed_groups.iter().any(|g| g == k) && s.collapsed_groups.len() < 100 {
                        s.collapsed_groups.push(k.to_owned());
                    }
                }
                "buddy" => {
                    if let Some(b) = Buddy::from_setting(v) {
                        let _ = buddies::add(&mut s.buddies, &b.nick, &b.group, &b.note);
                    }
                }
                "favorite" => {
                    if let Some(f) = FavServer::from_setting(v) {
                        s.favorites.add_server(f);
                    }
                }
                "highlight" => s.highlights.extend(Rule::from_setting(v)),
                "last_server" => s.last_server = v.into(),
                "last_port" => s.last_port = v.into(),
                "last_tls" => s.last_tls = b,
                "last_sasl_account" => s.last_sasl_account = v.into(),
                "last_autojoin" => s.last_autojoin = v.into(),
                _ => {}
            }
        }
        // `dock=` lines are the current format; otherwise the single dock of older files
        s.dock = Layout::from_dock_fields(dock_lines.iter().map(|l| l.as_str()))
            .unwrap_or_else(|| Layout::single(legacy.4, legacy.0, legacy.1, legacy.2, legacy.3));
        s
    }

    pub fn serialize(&self) -> String {
        let flag = |b: bool| if b { "1" } else { "0" };
        let mut out = format!(
            "nick={}\nuser={}\nrealname={}\nquit_msg={}\ntimestamps={}\nshow_toolbar={}\nshow_userlist={}\nhl_nick={}\nhl_flash={}\nhl_beep={}\nlogging={}\nlog_dir={}\nlast_server={}\nlast_port={}\nlast_tls={}\nlast_sasl_account={}\nlast_autojoin={}\n",
            one_line(&self.nick),
            one_line(&self.user),
            one_line(&self.realname),
            one_line(&self.quit_msg),
            flag(self.timestamps),
            flag(self.show_toolbar),
            flag(self.show_userlist),
            flag(self.hl_nick),
            flag(self.hl_flash),
            flag(self.hl_beep),
            flag(self.logging),
            one_line(&self.log_dir),
            one_line(&self.last_server),
            one_line(&self.last_port),
            flag(self.last_tls),
            one_line(&self.last_sasl_account),
            one_line(&self.last_autojoin),
        );
        out.push_str(&format!("dcc_ip={}\ndcc_ports={}\n", one_line(&self.dcc_ip), one_line(&self.dcc_ports)));
        out.push_str(&format!(
            "dcc_reject_exts={}\ndcc_max_mb={}\ndcc_reject_chat={}\ndcc_limit_kb={}\ndcc_upnp={}\nhistory_lines={}\ntyping_show={}\ntyping_send={}\nupdate_check={}\nupdate_last={}\n",
            one_line(&self.dcc_reject_exts),
            self.dcc_max_mb,
            flag(self.dcc_reject_chat),
            self.dcc_limit_kb,
            flag(self.dcc_upnp),
            self.history_on_join,
            flag(self.typing_show),
            flag(self.typing_send),
            flag(self.update_check),
            self.update_last
        ));
        for f in self.dock.dock_fields() {
            out.push_str("dock=");
            out.push_str(&f);
            out.push('\n');
        }
        if let Some(w) = &self.window {
            out.push_str("window=");
            out.push_str(&w.to_setting());
            out.push('\n');
        }
        for w in &self.columns {
            out.push_str("cols=");
            out.push_str(&w.to_setting());
            out.push('\n');
        }
        for c in &self.child_windows {
            out.push_str("child=");
            out.push_str(&c.to_setting());
            out.push('\n');
        }
        for (slot, l) in self.layouts.iter().enumerate() {
            if let Some(l) = l {
                out.push_str("layout=");
                out.push_str(&l.to_setting(slot));
                out.push('\n');
            }
        }
        out.push_str(&format!("client_cert={}\nclient_key={}\n", one_line(&self.client_cert), one_line(&self.client_key)));
        for p in &self.sts {
            out.push_str("sts=");
            out.push_str(&p.to_setting());
            out.push('\n');
        }
        for sv in &self.servers {
            out.push_str("server=");
            out.push_str(&sv.to_setting());
            out.push('\n');
        }
        out.push_str(&format!("theme={}\nchat_font={}\nui_font={}\n", self.theme.name(), one_line(&self.chat_font.to_setting()), one_line(&self.ui_font.to_setting())));
        for name in theme::COLOR_NAMES {
            if let Some(c) = self.colors.get(name) {
                out.push_str(&format!("color={name}\t{}\n", theme::hex(c)));
            }
        }
        for p in &self.perform {
            for line in &p.lines {
                out.push_str("perform=");
                out.push_str(&perform::to_setting(&one_line(&p.network), &one_line(line)));
                out.push('\n');
            }
        }
        for a in &self.aliases {
            out.push_str("alias=");
            out.push_str(&alias::to_setting(a));
            out.push('\n');
        }
        for i in &self.identities {
            out.push_str("identity=");
            out.push_str(&identity::to_setting(i));
            out.push('\n');
        }
        for (k, t) in &self.filters {
            out.push_str("filter=");
            out.push_str(&one_line(k));
            out.push('\t');
            out.push_str(&one_line(t));
            out.push('\n');
        }
        for g in &self.collapsed_groups {
            out.push_str("buddy_collapsed=");
            out.push_str(&one_line(g));
            out.push('\n');
        }
        for b in &self.buddies {
            out.push_str("buddy=");
            out.push_str(&b.to_setting());
            out.push('\n');
        }
        for e in &self.ignores {
            out.push_str("ignore=");
            out.push_str(&e.to_setting());
            out.push('\n');
        }
        for f in &self.favorites.servers {
            out.push_str("favorite=");
            out.push_str(&f.to_setting());
            out.push('\n');
        }
        for r in &self.highlights {
            out.push_str("highlight=");
            out.push_str(&r.to_setting());
            out.push('\n');
        }
        out
    }

    fn path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(base).join("ChimeraIRC").join("settings.ini"))
    }

    /// Where the settings were kept before the program was renamed from winIRC to ChimeraIRC.
    fn legacy_path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(base).join("winIRC").join("settings.ini"))
    }

    /// Settings written under the old name: the same format, except that the default quit message and real
    /// name were the program's name, which should follow it. Nothing else is changed.
    pub fn from_legacy(text: &str) -> Settings {
        let mut s = Settings::parse(text);
        if s.quit_msg == "winIRC" {
            s.quit_msg = "ChimeraIRC".into();
        }
        if s.realname == "winIRC" {
            s.realname = "ChimeraIRC".into();
        }
        s
    }

    /// The saved settings. Without any, the old program name's settings are taken over (and saved under the
    /// new name; the old file is left where it is), and without those the defaults are used.
    pub fn load() -> Settings {
        if let Some(t) = Self::path().and_then(|p| std::fs::read_to_string(p).ok()) {
            return Settings::parse(&t);
        }
        if let Some(t) = Self::legacy_path().and_then(|p| std::fs::read_to_string(p).ok()) {
            let s = Settings::from_legacy(&t);
            s.save();
            return s;
        }
        Settings::default()
    }

    pub fn save(&self) {
        if let Some(p) = Self::path() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, self.serialize());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layouts::Dock;
    use chimerairc_proto::format::Rgb;

    #[test]
    fn roundtrip() {
        let mut s = Settings::default();
        s.nick = "Alice".into();
        s.timestamps = false;
        s.show_toolbar = false;
        s.show_userlist = false;
        s.hl_nick = false;
        s.hl_flash = false;
        s.hl_beep = true;
        s.logging = true;
        s.log_dir = "D:\\chat logs".into();
        s.dcc_ip = "203.0.113.7".into();
        s.dcc_ports = "5000-5100".into();
        s.dcc_reject_exts = "exe,scr,risky".into();
        s.dcc_max_mb = 250;
        s.dcc_reject_chat = true;
        s.dcc_limit_kb = 512;
        s.dcc_upnp = true;
        s.history_on_join = 120;
        s.sts.push(StsPolicy { host: "irc.example.org".into(), port: 6697, expires: 1_900_000_000 });
        s.client_cert = "C:\\keys\\me.pem".into();
        s.client_key = "C:\\keys\\me.key".into();
        buddies::add(&mut s.buddies, "Alice", "Work", "met on #rust").unwrap();
        buddies::add(&mut s.buddies, "bob|away", "", "").unwrap();
        s.collapsed_groups = vec!["work".into(), buddies::UNGROUPED.into()];
        s.filters = vec![("users".into(), "alice #rust".into()), ("buddies".into(), "work".into())];
        s.theme = Choice::Dark;
        s.colors.set("bg", theme::parse_hex("#101820"));
        s.colors.set("notice", theme::parse_hex("#ffaa00"));
        s.chat_font = FontSpec::new("Cascadia Mono", 11, false);
        s.ui_font = FontSpec::new("Segoe UI", 10, true);
        perform::set(&mut s.perform, "Libera.Chat", "/msg NickServ identify {ask:NickServ password}\n/wait 2\n/join #rust").unwrap();
        perform::set(&mut s.perform, "irc.example.net", "/mode {nick} +i").unwrap();
        s.favorites.add_server(FavServer::new("Libera", "irc.libera.chat", 6697, true, &["#rust".to_owned(), "#linux".to_owned()]));
        s.favorites.add_server(FavServer::new("", "localhost", 6667, false, &[]));
        let mut ig = IgnoreEntry::new("|-|ydra");
        ig.notes = "noisy".into();
        ig.rule.channels = vec!["#a".into()];
        s.ignores.push(ig);
        let mut off = IgnoreEntry::new("*!*@evil.host");
        off.enabled = false;
        s.ignores.push(off);
        s.highlights = vec![
            Rule { word: "chimerairc".into(), case_sensitive: false, whole_word: true, whole_line: false, color: Rgb(0xc0, 0, 0) },
            Rule { word: "a|b".into(), case_sensitive: true, whole_word: false, whole_line: true, color: Rgb(0, 0x80, 0) },
        ];
        s.servers.push(ListedServer::new("My Net", "irc.example.org", 6697, true).unwrap());
        s.window = Some(Placement { left: -1200, top: 40, width: 900, height: 650, maximized: true });
        columns::set(&mut s.columns, "dock.buddies", &[100, 60, 70, 160]);
        columns::set(&mut s.columns, "chanlist", &[200, 80, 500]);
        winsizes::remember(&mut s.child_windows, ChildWindow { key: "ch:#rust".into(), left: 10, top: 20, width: 640, height: 480, maximized: false });
        winsizes::remember(&mut s.child_windows, ChildWindow { key: "q:bob".into(), left: 0, top: 0, width: 400, height: 300, maximized: true });
        s.dock = Layout::single(Edge::Bottom, false, 333, 0b0111_0101, 4);
        s.dock.docks[Edge::Right as usize] = Dock { visible: true, size: 260, panes: 0b1000_0000, active: 7 };
        s.dock = s.dock.normalized();
        s.layouts[0] = Some(Layout::single(Edge::Right, true, 250, 0b11, 1));
        s.layouts[3] = Some(Layout::default());
        s.last_autojoin = "#a,#b".into();
        s.last_tls = false;
        assert_eq!(Settings::parse(&s.serialize()), s);
    }

    #[test]
    fn settings_from_before_the_rename_are_taken_over() {
        let old = "nick=Alice\nquit_msg=winIRC\nrealname=winIRC\nbuddy=bob\tWork\t\ndock_edge=3\ndock_panes=255\n";
        let s = Settings::from_legacy(old);
        assert_eq!(s.nick, "Alice");
        assert_eq!((s.quit_msg.as_str(), s.realname.as_str()), ("ChimeraIRC", "ChimeraIRC"), "the default texts follow the name");
        assert_eq!(s.buddies.len(), 1, "everything else is kept");
        assert_eq!(s.dock.single_edge(), Some(Edge::Bottom));
        // texts the user chose themselves stay as they are
        let mine = Settings::from_legacy("quit_msg=Gone fishing\nrealname=Alice A.\n");
        assert_eq!((mine.quit_msg.as_str(), mine.realname.as_str()), ("Gone fishing", "Alice A."));
    }

    #[test]
    fn looks_are_saved_and_bad_values_fall_back_to_the_defaults() {
        let s = Settings::parse("theme=dark\ncolor=bg\t#000000\ncolor=fg\tnope\ncolor=info\t#0f0\ncolor=what\t#fff\nchat_font=Consolas|12|0\nui_font=junk\n");
        assert_eq!(s.theme, Choice::Dark);
        assert_eq!(s.colors.bg, theme::parse_hex("#000"));
        assert_eq!(s.colors.fg, None, "an unreadable colour is ignored");
        assert_eq!(s.colors.info, theme::parse_hex("#00ff00"));
        assert_eq!(s.chat_font, FontSpec::new("Consolas", 12, false));
        assert_eq!(s.ui_font, FontSpec::ui_default(), "a damaged font falls back");
        assert_eq!(Settings::parse("theme=sepia\n").theme, Choice::Light);
        let d = Settings::default();
        assert_eq!((d.theme, d.colors, d.chat_font.clone()), (Choice::Light, Overrides::default(), FontSpec::chat_default()));
    }

    #[test]
    fn typing_options_default_on_and_are_saved() {
        let d = Settings::default();
        assert!(d.typing_show && d.typing_send);
        assert!(Settings::parse("nick=x\n").typing_show, "a file without the lines keeps the defaults");
        let mut s = Settings::default();
        s.typing_send = false;
        let back = Settings::parse(&s.serialize());
        assert!(back.typing_show && !back.typing_send);
        s.typing_show = false;
        let back = Settings::parse(&s.serialize());
        assert!(!back.typing_show && !back.typing_send);
    }

    #[test]
    fn the_update_check_is_off_by_default_and_remembered() {
        let d = Settings::default();
        assert!(!d.update_check && d.update_last == 0);
        assert!(!Settings::parse("nick=x\n").update_check, "a file without the lines keeps the default: off");
        let mut s = Settings::default();
        s.update_check = true;
        s.update_last = 1_700_000_000;
        let back = Settings::parse(&s.serialize());
        assert!(back.update_check && back.update_last == 1_700_000_000);
        assert_eq!(Settings::parse("update_last=junk\n").update_last, 0);
    }

    #[test]
    fn tolerates_garbage_and_strips_newlines() {
        let mut s = Settings::default();
        s.realname = "evil\nnick=pwned".into();
        let back = Settings::parse(&s.serialize());
        assert_eq!(back.nick, Settings::default().nick);
        assert_eq!(back.realname, "evil nick=pwned");
        let g = Settings::parse("???\n=\nnick=\nunknown=1\ntimestamps=1");
        assert_eq!(g.nick, Settings::default().nick);
        assert!(g.timestamps);
    }

    #[test]
    fn the_dock_of_older_settings_files_is_still_read() {
        // before several docks: one dock described by five keys
        let old = Settings::parse("dock_visible=0\ndock_width=333\ndock_edge=3\ndock_pane=4\ndock_panes=117\n");
        assert_eq!(old.dock.single_edge(), Some(Edge::Bottom));
        let d = old.dock.dock(Edge::Bottom);
        assert_eq!((d.visible, d.size, d.panes, d.active), (false, 333, 117, 4));
        // before docking on other edges: no edge key, so left
        let older = Settings::parse("dock_visible=1\ndock_width=250\ndock_pane=1\ndock_panes=127\n");
        assert_eq!(older.dock.single_edge(), Some(Edge::Left));
        // nothing at all: the default dock
        assert_eq!(Settings::parse("").dock, Layout::default());
        // `dock=` lines win over the old keys
        let both = Settings::parse("dock_edge=3\ndock_panes=255\ndock=1,1,200,3,1\n");
        assert_eq!(both.dock.single_edge(), Some(Edge::Right));
        assert_eq!(both.dock.dock(Edge::Right).panes, 3);
        // junk dock lines fall back to the old keys / default instead of losing every pane
        assert_eq!(Settings::parse("dock=nonsense\n").dock, Layout::default());
        assert_eq!(Settings::parse("dock=0,1,200,0,0\n").dock.pane_count(), 1, "an empty dock is repaired");
    }
}
