//! Application state: connections, event routing and `/command` handling.
//! Everything here runs on the UI thread except the tokio forwarder tasks, which only
//! push into `QUEUE` and post `WM_APP_EVENT`.

use crate::dialogs::{self, Field};
use crate::chanlist;
use crate::dock;
use crate::history;
use crate::buddies;
use crate::perform;
use crate::stspolicy;
use crate::layouts::{self, Layout, Pane};
use crate::servers;
use crate::urls;
use crate::dccrules;
use crate::favdialog;
use crate::favorites;
use crate::igndialog;
use crate::ignorelist;
use crate::chanwin;
use crate::highlight;
use crate::hldialog;
use crate::logger;
use std::path::{Path, PathBuf};
use chimerairc_proto::format::{self, Rgb};
use crate::settings::Settings;
use crate::ui::{self, Kind, LineKind};
use crate::util::*;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tokio::runtime::Runtime;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;
use chimerairc_core::net::{self, Client};
use chimerairc_core::reconnect::{self, Backoff};
use chimerairc_core::{ChatKind, Config, Event};
use chimerairc_proto::Message;

struct Conn {
    client: Option<Client>,
    nick: String,
    host: String,
    port: u16,
    tls: bool,
    echo: bool,
    registered: bool,
    /// When the connection was registered (for `/uptime`).
    registered_at: Option<std::time::Instant>,
    /// We are marked away on this connection (the server said so with 306, and 305 when we came back).
    away: bool,
    server_win: isize,
    /// This connection's Channel List window (0 = none open).
    chanlist: isize,
    /// Channels to join once registered.
    autojoin: Vec<String>,
    /// Case-folded target -> window handle.
    wins: HashMap<String, isize>,
    /// How this connection was made (kept in memory only, passwords included, never saved), so it
    /// can be made again after a drop.
    last_cfg: Option<Config>,
    /// Channels to join again after a reconnect.
    rejoin: Vec<String>,
    /// The server demanded TLS (STS): reconnect on this port once the plaintext connection is gone.
    sts_upgrade: Option<u16>,
    /// The server offers chat history (`chathistory`).
    history: bool,
    /// The server lets us change our real name (`setname`).
    setname: bool,
    /// Message ids already shown, so replayed history is not printed twice.
    seen: history::Seen,
    /// Oldest history time shown per (lower-case) target: where `/history before` continues.
    history_oldest: HashMap<String, String>,
    /// Newest history time shown per (lower-case) target: where `/history after` continues.
    history_newest: HashMap<String, String>,
    /// Counts the runs of the auto-perform script of this connection, so that a pause that is still
    /// running when the connection is made again does not continue the old script.
    perform_gen: u32,
    /// Online state of the watched buddies on this connection: lower-case nick -> (online, mask).
    presence: HashMap<String, (bool, Option<String>)>,
    /// The user asked to disconnect: never reconnect on their own.
    want_quit: bool,
    /// This connection reached the point of being registered at least once.
    ever_registered: bool,
    /// A reconnect is in progress (waiting for a retry, or trying one).
    retrying: bool,
    backoff: Backoff,
    /// Labeled replies, and the recent messages that can be answered or reacted to (see `ircv3.rs`).
    v3: crate::ircv3::State,
    /// Bumped whenever the user connects or quits, so a retry timer that is already running is ignored.
    retry_gen: u32,
}

struct App {
    rt: Runtime,
    conns: HashMap<u32, Conn>,
    next: u32,
    settings: Settings,
    logger: logger::Logger,
    dcc: dcc::State,
}

mod dcc;
mod scripts;
mod v3;

static QUEUE: Mutex<VecDeque<(u32, Event)>> = Mutex::new(VecDeque::new());
/// Reconnect timers that have run out: (connection id, retry generation).
static RETRIES: Mutex<VecDeque<(u32, u32)>> = Mutex::new(VecDeque::new());
/// Auto-perform scripts that are due: (connection id, run number, the steps left).
static PERFORM: Mutex<VecDeque<(u32, u32, Vec<PerformStep>)>> = Mutex::new(VecDeque::new());

/// One step of a running auto-perform script.
#[derive(Clone, Debug)]
enum PerformStep {
    /// A command line as typed (placeholders not yet filled in).
    Line(String),
    /// Channels to join (the favorite's list, after the script has done its work).
    Join(Vec<String>),
}
static POSTED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut App) -> R) -> R {
    APP.with(|a| f(a.borrow_mut().as_mut().expect("app initialised")))
}

/// Tear the runtime down without waiting on blocking threads (dropping a Runtime from a
/// thread-local destructor can hang at process exit).
pub fn shutdown_runtime() {
    if let Some(app) = APP.with(|a| a.borrow_mut().take()) {
        app.rt.shutdown_background();
    }
}

pub fn init() {
    // (settings are loaded below; menu check state is synced after the frame exists)
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let settings = Settings::load();
    chimerairc_core::dcc::set_rate_limit(settings.dcc_limit_kb.saturating_mul(1024));
    APP.with(|a| *a.borrow_mut() = Some(App { rt, conns: HashMap::new(), next: 1, settings, logger: logger::Logger::default(), dcc: dcc::State::default() }));
}

fn settings() -> Settings {
    with(|a| a.settings.clone())
}

fn quit_msg() -> String {
    with(|a| a.settings.quit_msg.clone())
}

pub fn sync_menu() {
    let (ts, tb, ul, lg) =
        with(|a| (a.settings.timestamps, a.settings.show_toolbar, a.settings.show_userlist, a.settings.logging));
    ui::set_timestamps_checked(ts);
    let (ts_show, ts_send) = with(|a| (a.settings.typing_show, a.settings.typing_send));
    ui::set_typing_checked(ts_show, ts_send);
    ui::set_update_checked(with(|a| a.settings.update_check));
    sync_favorites();
    ui::apply_view(tb, ul, lg);
    ui::apply_dock(dock_layout());
    refresh_server_list();
    refresh_buddies();
    refresh_monitors();
}

pub fn set_show_toolbar(show: bool) {
    with(|a| {
        a.settings.show_toolbar = show;
        a.settings.save();
    });
    sync_menu();
}

pub fn set_show_userlist(show: bool) {
    with(|a| {
        a.settings.show_userlist = show;
        a.settings.save();
    });
    sync_menu();
}

pub fn nick_of(conn: u32) -> String {
    with(|a| a.conns.get(&conn).map(|c| c.nick.clone()).unwrap_or_default())
}

/// Run an action chosen from the user-list menu (or a double-click) on `nick`, in the
/// channel window `h`.
pub fn user_action(h: HWND, action: ui::UserAction, nick: String) {
    use ui::UserAction::*;
    let Some((conn, chan)) = ui::child(h).map(|c| (c.conn, c.name.clone())) else { return };
    if action == CopyNick {
        ui::copy_to_clipboard(&nick);
        return;
    }
    let me = nick_of(conn);
    match action {
        SendFile => return dcc::send_file_dialog(h, conn, &nick),
        DccChat => return dcc::chat_start(h, conn, &nick),
        Ignore => return ignore_add(h, conn, &nick),
        Unignore => return ignore_remove(h, &nick),
        AddBuddy => return buddy_add(h, &nick, ""),
        RemoveBuddy => return buddy_remove(h, &nick),
        _ => {}
    }
    if action == Query {
        if !eq_ic(&nick, &me) {
            if let Some(qh) = window_for(conn, Kind::Query, &nick, true) {
                ui::activate(hw(qh));
            }
        }
        return;
    }
    if !with(|a| a.conns.get(&conn).is_some_and(|c| c.client.is_some())) {
        local(h, "* Not connected.");
        return;
    }
    let ctcp = |what: &str| {
        let body = if what == "PING" {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("\x01PING {secs}\x01")
        } else {
            format!("\x01{what}\x01")
        };
        send(conn, Message::new("PRIVMSG", &[&nick, &body]));
        local(h, &format!("* CTCP {what} request sent to {nick}"));
    };
    let mode = |m: &str| {
        send(conn, Message::new("MODE", &[&chan, m, &nick]));
    };
    let kick = || {
        send(conn, Message::new("KICK", &[&chan, &nick, "Kicked"]));
    };
    let ban = || {
        send(conn, Message::new("MODE", &[&chan, "+b", &format!("{nick}!*@*")]));
    };
    match action {
        Whois => {
            send(conn, Message::new("WHOIS", &[&nick]));
        }
        CtcpVersion => ctcp("VERSION"),
        CtcpPing => ctcp("PING"),
        CtcpTime => ctcp("TIME"),
        Op => mode("+o"),
        Deop => mode("-o"),
        Voice => mode("+v"),
        Devoice => mode("-v"),
        Kick => kick(),
        Ban => ban(),
        KickBan => {
            ban();
            kick();
        }
        Query | CopyNick | Ignore | Unignore | SendFile | DccChat | AddBuddy | RemoveBuddy => {}
    }
}

fn is_channel(s: &str) -> bool {
    s.starts_with(['#', '&', '+', '!'])
}

fn eq_ic(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

// ------------------------------------------------------------------ windows

pub fn new_server_window() -> HWND {
    let nick = settings().nick; // before `with`: the app cell must not be borrowed twice
    let id = with(|a| {
        let id = a.next;
        a.next += 1;
        a.conns.insert(
            id,
            Conn {
                client: None,
                nick: nick.clone(),
                host: String::new(),
                port: 0,
                tls: true,
                echo: false,
                registered: false,
                registered_at: None,
                away: false,
                server_win: 0,
                chanlist: 0,
                autojoin: Vec::new(),
                wins: HashMap::new(),
                last_cfg: None,
                rejoin: Vec::new(),
                sts_upgrade: None,
                presence: HashMap::new(),
                perform_gen: 0,
                history: false,
                setname: false,
                seen: history::Seen::default(),
                history_oldest: HashMap::new(),
                history_newest: HashMap::new(),
                want_quit: false,
                ever_registered: false,
                retrying: false,
                backoff: Backoff::default(),
                v3: crate::ircv3::State::default(),
                retry_gen: 0,
            },
        );
        id
    });
    let h = ui::create_child(Kind::Server, id, "Status");
    with(|a| {
        if let Some(c) = a.conns.get_mut(&id) {
            c.server_win = raw(h);
        }
    });
    print(raw(h), "ChimeraIRC - type /help for commands, /server irc.libera.chat to connect (TLS, port 6697).");
    h
}

/// Find (optionally creating) the window for a channel or query.
fn window_for(conn: u32, kind: Kind, name: &str, create: bool) -> Option<isize> {
    window_for_ex(conn, kind, name, create, true)
}

/// Names of this connection's channel windows (for tab completion).
pub fn channel_names(conn: u32) -> Vec<String> {
    let wins: Vec<isize> = with(|a| a.conns.get(&conn).map(|c| c.wins.values().copied().collect()).unwrap_or_default());
    wins.into_iter().filter_map(|h| ui::child_name(hw(h), Kind::Channel)).collect()
}

/// Like `window_for`; with `take_focus == false` a newly created window does not steal focus.
fn window_for_ex(conn: u32, kind: Kind, name: &str, create: bool, take_focus: bool) -> Option<isize> {
    let key = name.to_lowercase();
    if let Some(h) = with(|a| a.conns.get(&conn).and_then(|c| c.wins.get(&key).copied())) {
        return Some(h);
    }
    if !create || with(|a| !a.conns.contains_key(&conn)) {
        return None;
    }
    let h = raw(ui::create_child_ex(kind, conn, name, take_focus));
    with(|a| {
        if let Some(c) = a.conns.get_mut(&conn) {
            c.wins.insert(key, h);
        }
    });
    Some(h)
}

fn print_k(h: isize, kind: LineKind, line: &str) {
    print_marked(h, kind, line, &highlight::Marks::default());
}

fn print_marked(h: isize, kind: LineKind, line: &str, marks: &highlight::Marks) {
    print_marked_at(h, kind, line, marks, None);
}

/// Like `print_marked`, with the time the server says the line was sent (`server-time`) instead of now.
fn print_marked_at(h: isize, kind: LineKind, line: &str, marks: &highlight::Marks, sent: Option<&str>) {
    let ts = with(|a| a.settings.timestamps).then(|| sent.map(str::to_owned).unwrap_or_else(timestamp));
    ui::print(hw(h), ts.as_deref(), line, kind, marks);
    log_line(h, line);
    catch_urls(h, line);
    feed_output(h, line);
}

/// Every line printed in any window also goes to the Output pane, labelled with where it came from.
fn feed_output(h: isize, line: &str) {
    let Some((conn, kind, name)) = ui::window_label(hw(h)) else { return };
    let host = with(|a| a.conns.get(&conn).map(|c| c.host.clone())).unwrap_or_default();
    let place = match kind {
        Kind::Server => host,
        _ if host.is_empty() => name,
        _ => format!("{host} {name}"),
    };
    let stamp = datetime_stamp();
    dock::output(&format!("[{}] {}: {}", stamp.get(11..).unwrap_or(&stamp), place, format::strip(line)));
}

/// Active highlight rules: our own nick (if enabled) first, then the user's, so theirs win.
fn highlight_rules(my_nick: &str) -> Vec<highlight::Rule> {
    with(|a| {
        let mut rules = Vec::new();
        if a.settings.hl_nick && !my_nick.is_empty() {
            rules.push(highlight::Rule { whole_word: true, whole_line: true, ..highlight::Rule::new(my_nick, Rgb(0xc0, 0x00, 0x00)) });
        }
        rules.extend(a.settings.highlights.iter().cloned());
        rules
    })
}

/// Taskbar flash and sound for a highlight; `own` is the sound of the rule that matched, if it has one.
fn notify_highlight(own: &str) {
    if with(|a| a.settings.hl_flash) && !ui::app_is_foreground() {
        ui::flash_taskbar();
    }
    play_sound("highlight", own);
}

/// Play the sound for an event. A sound a highlight rule asks for (`own`) beats the event's own choice. With
/// "only in the background" on (the default) nothing plays while ChimeraIRC is the window in use.
pub fn play_sound(event: &str, own: &str) {
    let (chosen, background_only) = with(|a| {
        let own = own.trim();
        (if own.is_empty() { a.settings.sound_for(event).to_owned() } else { own.to_owned() }, a.settings.sounds_background)
    });
    if background_only && ui::app_is_foreground() {
        return;
    }
    crate::sounds::play(&chosen);
}

/// Status-style line (grey).
fn print(h: isize, line: &str) {
    print_k(h, LineKind::Info, line);
}

fn server_print(conn: u32, line: &str) {
    if let Some(h) = with(|a| a.conns.get(&conn).map(|c| c.server_win)) {
        if h != 0 {
            print(h, line);
        }
    }
    // the same line goes to the Event Log pane, tagged with the server it is about
    let host = with(|a| a.conns.get(&conn).map(|c| c.host.clone())).unwrap_or_default();
    let stamp = datetime_stamp();
    let label = if host.is_empty() { String::new() } else { format!("{host}  ") };
    dock::event(&format!("[{}] {label}{}", stamp.get(11..).unwrap_or(&stamp), line.trim_start_matches("* ")));
}

fn all_windows(conn: u32) -> Vec<isize> {
    with(|a| {
        a.conns
            .get(&conn)
            .map(|c| c.wins.values().copied().chain(std::iter::once(c.server_win)).filter(|h| *h != 0).collect())
            .unwrap_or_default()
    })
}

pub fn window_closed(h: isize) {
    with(|a| {
        for c in a.conns.values_mut() {
            c.wins.retain(|_, v| *v != h);
            if c.server_win == h {
                c.server_win = 0;
            }
        }
    });
}

/// Called from WM_CLOSE of a child, before it is destroyed.
pub fn child_closing(h: HWND) {
    let Some((kind, conn, name)) = ui::child(h).map(|c| (c.kind, c.conn, c.name.clone())) else { return };
    match kind {
        Kind::Channel => {
            send(conn, Message::new("PART", &[&name]));
        }
        Kind::Query => {}
        Kind::DccChat => dcc::chat_closed(h),
        Kind::Server => {
            // Closing the server window ends the connection and closes its channel/query windows.
            let (client, others) = with(|a| match a.conns.remove(&conn) {
                Some(c) => {
                    let mut windows: Vec<isize> = c.wins.into_values().collect();
                    windows.extend((c.chanlist != 0).then_some(c.chanlist));
                    (c.client, windows)
                }
                None => (None, vec![]),
            });
            if let Some(cl) = client {
                cl.quit(Some(&quit_msg()));
            }
            for o in others {
                unsafe {
                    let _ = DestroyWindow(hw(o));
                }
            }
        }
    }
}

pub fn shutdown() {
    let clients: Vec<Client> = with(|a| a.conns.values_mut().filter_map(|c| c.client.take()).collect());
    for c in &clients {
        c.quit(Some(&quit_msg()));
    }
    // Give QUIT a moment to flush before the process exits.
    if !clients.is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}

pub fn refresh_status() {
    let info = ui::active_child().and_then(|h| ui::child(h).map(|c| c.conn));
    let (state, nick, host) = match info.and_then(|id| with(|a| a.conns.get(&id).map(|c| {
        let st = match (&c.client, c.registered) {
            (None, _) if c.retrying => "Reconnecting...",
            (None, _) => "Disconnected",
            (Some(_), false) => "Connecting...",
            (Some(_), true) => "Connected",
        };
        (st.to_owned(), c.nick.clone(), c.host.clone())
    }))) {
        Some(v) => v,
        None => ("Ready".to_owned(), String::new(), String::new()),
    };
    let typing = if with(|a| a.settings.typing_show) { ui::active_child().map(ui::typing_text).unwrap_or_default() } else { String::new() };
    ui::set_status(0, &if typing.is_empty() { state } else { format!("{state}    {typing}") });
    ui::set_status(1, &nick);
    ui::set_status(2, &host);
}

// ------------------------------------------------------------------ connection

fn send(conn: u32, m: Message) -> bool {
    with(|a| a.conns.get(&conn).and_then(|c| c.client.as_ref()).map(|c| c.send(m))).unwrap_or(false)
}

/// The user wants out: no automatic reconnecting from now on, and a retry timer already running is
/// disarmed. Returns true if one was pending.
fn stop_reconnecting(conn: u32) -> bool {
    with(|a| {
        a.conns.get_mut(&conn).is_some_and(|c| {
            c.want_quit = true;
            c.retry_gen = c.retry_gen.wrapping_add(1);
            std::mem::replace(&mut c.retrying, false)
        })
    })
}

pub fn disconnect(conn: u32) {
    let was_retrying = stop_reconnecting(conn);
    let c = with(|a| a.conns.get(&conn).and_then(|c| c.client.clone()));
    if was_retrying && c.is_none() {
        server_print(conn, "* Reconnecting cancelled.");
        return;
    }
    match c {
        Some(c) => {
            c.quit(Some(&quit_msg()));
        }
        None => server_print(conn, "* Not connected."),
    }
}

// ------------------------------------------------------------------ update check

/// The GitHub repository whose releases are looked at.
const UPDATE_REPO: &str = "theG33K/chimerairc";

/// The answer of the check that is on its way: (the user asked for it, what was found).
static UPDATE_ANSWER: Mutex<Option<(bool, Result<chimerairc_core::update::Outcome, String>)>> = Mutex::new(None);

/// The version the program says it is (a test can pretend with `CHIMERAIRC_TEST_VERSION`).
fn current_version() -> String {
    std::env::var("CHIMERAIRC_TEST_VERSION").ok().filter(|v| chimerairc_core::update::parse_version(v).is_some()).unwrap_or_else(|| crate::about::VERSION.to_owned())
}

/// Ask GitHub for the release list in the background; the answer comes back as `WM_APP_UPDATE`.
fn start_update_check(manual: bool) {
    let frame = raw(ui::frame());
    let current = current_version();
    with(|a| {
        let _guard = a.rt.enter();
        a.rt.spawn(async move {
            let result = chimerairc_core::update::check_github(UPDATE_REPO, &current).await;
            *UPDATE_ANSWER.lock().unwrap() = Some((manual, result));
            unsafe {
                let _ = PostMessageW(hw(frame), ui::WM_APP_UPDATE, WPARAM(0), LPARAM(0));
            }
        });
    });
}

/// Help > Check for Updates...
pub fn check_updates_now() {
    start_update_check(true);
}

/// Options > Check for Updates at Start: off unless the user turns it on.
pub fn toggle_update_check() {
    let on = with(|a| {
        a.settings.update_check = !a.settings.update_check;
        a.settings.save();
        a.settings.update_check
    });
    sync_menu();
    if on {
        dialogs::info(
            ui::frame(),
            "Check for updates",
            "ChimeraIRC will now look for a newer release on GitHub when it starts, at most once a day. The request carries \
             only the program name and version. Nothing is ever downloaded or installed by itself.",
        );
    }
}

/// At start-up: when the user has switched the check on and a day has passed, look quietly.
pub fn scripts_start() {
    scripts::start();
}

pub fn scripts_tick() {
    scripts::tick();
}

pub fn scripts_reload_command() {
    if let Some(h) = ui::active_child() {
        scripts::reload(raw(h));
    }
}

pub fn scripts_open_folder() {
    scripts::open_folder();
}

pub fn update_check_on_start() {
    let due = with(|a| {
        let now = unix_now();
        let due = a.settings.update_check && now.saturating_sub(a.settings.update_last) >= 24 * 3600;
        if due {
            a.settings.update_last = now;
            a.settings.save();
        }
        due
    });
    if due {
        start_update_check(false);
    }
}

/// The check has an answer: tell the user (a question and a page to open when they asked; one line otherwise).
pub fn update_answer() {
    use chimerairc_core::update::Outcome;
    let Some((manual, result)) = UPDATE_ANSWER.lock().unwrap().take() else { return };
    let owner = ui::frame();
    let current = current_version();
    match result {
        Ok(Outcome::Newer(r)) => {
            let kind = if r.prerelease { " (pre-release)" } else { "" };
            if manual {
                let text = format!("ChimeraIRC {} is available{kind}. You have {current}.\n\nOpen the download page?", r.tag.trim_start_matches(['v', 'V']));
                if dialogs::confirm(owner, "Check for updates", &text) {
                    open_release_page(&r.url);
                }
            } else if let Some(h) = ui::active_child() {
                ui::print(h, None, &format!("* ChimeraIRC {} is available{kind} (you have {current}): Help > Check for Updates...", r.tag.trim_start_matches(['v', 'V'])), LineKind::Info, &highlight::Marks::default());
            }
        }
        Ok(Outcome::UpToDate { .. }) => {
            if manual {
                dialogs::info(owner, "Check for updates", &format!("You have the latest version of ChimeraIRC ({current})."));
            }
        }
        Err(e) => {
            if manual {
                dialogs::message(owner, "Check for updates", &format!("The check could not be made: {e}."));
            }
        }
    }
}

/// Open a release page, but only one of this project's own on github.com.
fn open_release_page(url: &str) {
    let ok = url.starts_with(&format!("https://github.com/{UPDATE_REPO}/")) && !url.contains(char::is_whitespace);
    if ok {
        shell_open_url(url);
    } else {
        dialogs::message(ui::frame(), "Check for updates", "The release page address was not one of this project's, so it was not opened.");
    }
}

fn shell_open_url(url: &str) {
    let u = wide(url);
    unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(None, windows::core::w!("open"), windows::core::PCWSTR(u.as_ptr()), None, None, SW_SHOWNORMAL);
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The TLS client certificate from Preferences, and (advanced) extra trusted roots from the
/// environment variable `CHIMERAIRC_EXTRA_ROOT` (a PEM file: a private server's own certificate
/// authority or self-signed certificate).
fn apply_tls_extras(conn: u32, cfg: &mut Config) {
    let st = settings();
    let cert_path = st.client_cert.trim();
    if !cert_path.is_empty() && cfg.client_cert.is_none() {
        if !cfg.tls {
            server_print(conn, "* Your client certificate is not used: TLS is off for this connection.");
        } else {
            let key = (!st.client_key.trim().is_empty()).then(|| PathBuf::from(st.client_key.trim()));
            match chimerairc_core::clientcert::load(Path::new(cert_path), key.as_deref()) {
                Ok(c) => {
                    server_print(conn, &format!("* Using your client certificate (SHA-256 {}).", c.fingerprint()));
                    cfg.client_cert = Some(c);
                }
                Err(e) => server_print(conn, &format!("* Your client certificate was not used: {e}.")),
            }
        }
    }
    if let Ok(path) = std::env::var("CHIMERAIRC_EXTRA_ROOT") {
        match chimerairc_core::clientcert::load_roots(Path::new(&path)) {
            Ok(roots) => cfg.extra_roots = roots,
            Err(e) => server_print(conn, &format!("* CHIMERAIRC_EXTRA_ROOT was not used: {e}.")),
        }
    }
}

/// The server announced (over verified TLS) how long we must use TLS for it.
fn on_sts_policy(conn: u32, duration: u64, port: u16) {
    let Some(host) = with(|a| a.conns.get(&conn).map(|c| c.host.clone())).filter(|h| !h.is_empty()) else { return };
    let now = unix_now();
    let change = with(|a| {
        stspolicy::prune(&mut a.settings.sts, now);
        let c = stspolicy::set(&mut a.settings.sts, &host, port, duration, now);
        if c != stspolicy::Change::Nothing {
            a.settings.save();
        }
        c
    });
    match change {
        stspolicy::Change::New => server_print(
            conn,
            &format!("* STS: {host} must be reached over TLS; remembered for {} days.", duration / 86_400),
        ),
        stspolicy::Change::Removed => server_print(conn, &format!("* STS: the policy for {host} was withdrawn.")),
        _ => {}
    }
}

/// The plaintext connection that asked for TLS has ended: connect again over TLS on the port the
/// server named, with the same settings.
fn sts_reconnect(conn: u32, port: u16) {
    let job = with(|a| {
        a.conns.get_mut(&conn).and_then(|c| {
            let mut cfg = c.last_cfg.clone()?;
            cfg.tls = true;
            cfg.port = port;
            c.last_cfg = Some(cfg.clone());
            Some((cfg, c.autojoin.clone()))
        })
    });
    if let Some((cfg, autojoin)) = job {
        connect_now(conn, cfg, autojoin);
    }
}

/// A connection the user asked for (as opposed to an automatic retry): resets the reconnect state.
fn connect(conn: u32, cfg: Config, autojoin: Vec<String>) {
    if with(|a| a.conns.get(&conn).is_some_and(|c| c.client.is_some())) {
        server_print(conn, "* Already connected. Use /quit first.");
        return;
    }
    with(|a| {
        if let Some(c) = a.conns.get_mut(&conn) {
            c.last_cfg = Some(cfg.clone());
            c.want_quit = false;
            c.ever_registered = false;
            c.retrying = false;
            c.backoff.reset();
            c.retry_gen = c.retry_gen.wrapping_add(1);
        }
    });
    connect_now(conn, cfg, autojoin);
}

fn connect_now(conn: u32, mut cfg: Config, autojoin: Vec<String>) {
    if with(|a| a.conns.get(&conn).is_some_and(|c| c.client.is_some())) {
        server_print(conn, "* Already connected. Use /quit first.");
        return;
    }
    cfg.ignores = ignore_rules();
    // Strict Transport Security: a host that promised TLS is never contacted without it.
    if !cfg.tls {
        let now = unix_now();
        let policy = with(|a| stspolicy::lookup(&a.settings.sts, &cfg.host, now).cloned());
        if let Some(p) = policy {
            server_print(conn, &format!("* STS: {} requires TLS; connecting securely on port {} instead of {}.", cfg.host, p.port, cfg.port));
            cfg.tls = true;
            cfg.port = p.port;
        }
    }
    apply_tls_extras(conn, &mut cfg);
    let (host, nick, port, tls) = (cfg.host.clone(), cfg.nick.clone(), cfg.port, cfg.tls);
    // Set the host first so the "Connecting..." line below is filed under it in the log.
    with(|a| {
        if let Some(c) = a.conns.get_mut(&conn) {
            c.host = host.clone();
            c.port = port;
            c.tls = tls;
        }
    });
    server_print(conn, &format!("* Connecting to {host}:{port} ({})...", if tls { "TLS" } else { "plaintext" }));
    with(|a| {
        let _guard = a.rt.enter();
        let (client, mut rx) = net::spawn(cfg);
        let watch = buddies::nicks(&a.settings.buddies);
        if !watch.is_empty() {
            client.watch(watch);
        }
        a.rt.spawn(async move {
            while let Some(e) = rx.recv().await {
                QUEUE.lock().unwrap().push_back((conn, e));
                wake_ui();
            }
        });
        if let Some(c) = a.conns.get_mut(&conn) {
            c.client = Some(client);
            c.presence.clear();
            c.registered = false;
            c.nick = nick;
            c.host = host;
            c.autojoin = autojoin;
        }
    });
    refresh_status();
}

fn base_config(host: &str, port: u16, tls: bool, nick: &str) -> Config {
    let st = settings();
    let mut cfg = Config::new(host, nick);
    cfg.time_text = crate::util::local_ctime;
    cfg.history_on_join = st.history_on_join;
    cfg.port = port;
    cfg.tls = tls;
    cfg.user = st.user;
    cfg.realname = st.realname;
    cfg
}

fn parse_channels(s: &str) -> Vec<String> {
    s.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|c| !c.is_empty())
        .map(|c| if is_channel(c) { c.to_owned() } else { format!("#{c}") })
        .collect()
}

/// Run a dialog helper from the frame, never while holding the app borrow.
pub fn connect_dialog() {
    let owner = ui::frame();
    let s = settings();
    let ident = identity_for(&s.last_server);
    let mut fields = vec![
        Field::text("Server", &s.last_server),
        Field::number("Port", &s.last_port),
        Field::check("Use TLS (recommended)", s.last_tls),
        Field::text("Nickname", ident.as_ref().map(|i| i.nick.as_str()).filter(|n| !n.is_empty()).unwrap_or(&s.nick)),
        Field::text("Username", ident.as_ref().map(|i| i.user.as_str()).filter(|n| !n.is_empty()).unwrap_or(&s.user)),
        Field::text("Real name", ident.as_ref().map(|i| i.realname.as_str()).filter(|n| !n.is_empty()).unwrap_or(&s.realname)),
        Field::password("Server password"),
        Field::text("SASL account", ident.as_ref().map(|i| i.sasl_account.as_str()).filter(|n| !n.is_empty()).unwrap_or(&s.last_sasl_account)),
        Field::password("SASL password"),
        Field::text("Auto-join channels", &s.last_autojoin),
    ];
    loop {
        let Some(v) = dialogs::show(owner, "Connect to server", &fields) else { return };
        let (host, nick) = (v[0].trim().to_owned(), v[3].trim().to_owned());
        let port = v[1].trim().parse::<u16>().ok().filter(|p| *p > 0);
        let tls = v[2] == "1";
        let (spass, sacct, sapass) = (v[6].clone(), v[7].trim().to_owned(), v[8].clone());
        let problem = if host.is_empty() || host.contains(char::is_whitespace) {
            Some("Enter a server host name.")
        } else if port.is_none() {
            Some("Port must be a number from 1 to 65535.")
        } else if nick.is_empty() || nick.contains(char::is_whitespace) {
            Some("Enter a nickname (no spaces).")
        } else if !sacct.is_empty() && sapass.is_empty() {
            Some("Enter the SASL password, or clear the SASL account.")
        } else {
            None
        };
        let keep = |fields: &mut Vec<Field>, v: &[String]| {
            for (f, val) in fields.iter_mut().zip(v) {
                f.value = val.clone();
            }
        };
        if let Some(msg) = problem {
            dialogs::message(owner, "Connect", msg);
            keep(&mut fields, &v);
            continue;
        }
        let has_secret = !spass.is_empty() || !sacct.is_empty();
        if !tls && has_secret && !dialogs::confirm(owner, "Unencrypted connection",
            "TLS is off, so everything you send, and what comes back, is readable by anyone on the network. \
             A server password would be sent in clear text. A SASL login uses SCRAM-SHA-256 when the server \
             offers it (the password is then never sent), otherwise PLAIN, which sends the password in clear text.\n\nSend it anyway?")
        {
            keep(&mut fields, &v);
            continue;
        }

        let mut cfg = base_config(&host, port.unwrap(), tls, &nick);
        if let Some(i) = identity_for(&host) {
            cfg.alt_nicks = i.alt_nicks;
        }
        if !v[4].trim().is_empty() {
            cfg.user = v[4].trim().to_owned();
        }
        if !v[5].trim().is_empty() {
            cfg.realname = v[5].trim().to_owned();
        }
        if !spass.is_empty() {
            cfg.password = Some(spass);
        }
        if !sacct.is_empty() {
            cfg.sasl = Some((sacct.clone(), sapass));
        }
        with(|a| {
            let st = &mut a.settings;
            st.last_server = host.clone();
            st.last_port = port.unwrap().to_string();
            st.last_tls = tls;
            st.nick = nick.clone();
            st.user = cfg.user.clone();
            st.realname = cfg.realname.clone();
            st.last_sasl_account = sacct;
            st.last_autojoin = v[9].trim().to_owned();
            st.save();
        });

        connect(connection_for_new_server(), cfg, parse_channels(&v[9]));
        return;
    }
}

/// The connection a new server should use: the active window's if it is idle, else a new server window.
fn connection_for_new_server() -> u32 {
    let idle = ui::active_child()
        .and_then(|h| ui::child(h).map(|c| c.conn))
        .filter(|id| with(|a| a.conns.get(id).is_some_and(|c| c.client.is_none())));
    idle.unwrap_or_else(|| {
        let h = new_server_window();
        ui::child(h).map(|c| c.conn).unwrap_or(0)
    })
}

// ------------------------------------------------------------------ dock and server list

// ------------------------------------------------------------------ dock panes: feeds, monitors, actions

/// `HH:MM:SS`, for the rows of the list panes.
fn clock() -> String {
    let s = datetime_stamp();
    s.get(11..).unwrap_or(&s).to_owned()
}

fn feed_notice(from: &str, to: &str, text: &str) {
    dock::push_row(
        Pane::Notices,
        dock::Row { cells: vec![clock(), from.to_owned(), to.to_owned(), format::strip(text)], target: dock::Target::None },
    );
}

fn feed_message(conn: u32, nick: &str, text: &str, action: bool) {
    let body = format::strip(text);
    let shown = if action { format!("* {nick} {body}") } else { body };
    dock::push_row(
        Pane::Messages,
        dock::Row { cells: vec![clock(), nick.to_owned(), shown], target: dock::Target::Query { conn, nick: nick.to_owned() } },
    );
}

/// Every web address printed in any window goes into the URL Catcher.
fn catch_urls(h: isize, line: &str) {
    if !line.contains("://") {
        return; // the common case: nothing to look for
    }
    let place = ui::child(hw(h))
        .map(|c| if c.kind == Kind::Server { "status".to_owned() } else { c.name.clone() })
        .unwrap_or_default();
    for u in urls::extract(line) {
        dock::push_row(Pane::Urls, dock::Row { cells: vec![clock(), u.clone(), place.clone()], target: dock::Target::Url(u) });
    }
}

/// Rebuild the Channels and Connections monitors from the live state (a pane whose rows did not
/// change is left alone).
pub fn refresh_monitors() {
    struct Snap {
        id: u32,
        host: String,
        nick: String,
        connected: bool,
        registered: bool,
        retrying: bool,
        wins: Vec<isize>,
    }
    let mut snaps: Vec<Snap> = with(|a| {
        a.conns
            .iter()
            .map(|(id, c)| Snap {
                id: *id,
                host: c.host.clone(),
                nick: c.nick.clone(),
                connected: c.client.is_some(),
                registered: c.registered,
                retrying: c.retrying,
                wins: c.wins.values().copied().collect(),
            })
            .collect()
    });
    snaps.sort_by_key(|s| s.id);
    let mut conn_rows = Vec::new();
    let mut chan_rows = Vec::new();
    for s in &snaps {
        let host = if s.host.is_empty() { "(not connected)".to_owned() } else { s.host.clone() };
        let state = match (s.connected, s.registered, s.retrying) {
            (false, _, true) => "Reconnecting",
            (false, _, false) => "Disconnected",
            (true, false, _) => "Connecting",
            (true, true, _) => "Connected",
        };
        let mut channels: Vec<(String, usize, String)> =
            s.wins.iter().filter_map(|h| ui::channel_info(hw(*h))).collect();
        channels.sort_by_key(|(name, _, _)| name.to_lowercase());
        conn_rows.push(dock::Row {
            cells: vec![host.clone(), s.nick.clone(), state.to_owned(), channels.len().to_string()],
            target: dock::Target::Conn(s.id),
        });
        for (name, users, topic) in channels {
            chan_rows.push(dock::Row {
                cells: vec![host.clone(), name.clone(), users.to_string(), format::strip(&topic)],
                target: dock::Target::Channel { conn: s.id, name },
            });
        }
    }
    dock::set_rows(Pane::Connections, conn_rows);
    dock::set_rows(Pane::Channels, chan_rows);
    if dock::is_front(Pane::Users) {
        refresh_users(&snaps.iter().map(|s| (s.id, s.host.clone(), s.wins.clone())).collect::<Vec<_>>());
    }
}

/// Most people the Users pane lists (a very busy network could have far more than a list should hold).
const USER_ROWS_MAX: usize = 20_000;

/// Rebuild the global user list: everyone in every channel of every connection, once per connection, with
/// the channels they share with us.
fn refresh_users(conns: &[(u32, String, Vec<isize>)]) {
    use std::collections::BTreeMap;
    // (connection, lower-case nick) -> (nick as seen, channels)
    let mut people: BTreeMap<(u32, String), (String, Vec<String>)> = BTreeMap::new();
    for (conn, _, wins) in conns {
        for w in wins {
            let Some((name, _, _)) = ui::channel_info(hw(*w)) else { continue };
            for (_, nick) in ui::channel_members(hw(*w)) {
                let e = people.entry((*conn, nick.to_lowercase())).or_insert_with(|| (nick.clone(), Vec::new()));
                if !e.1.contains(&name) {
                    e.1.push(name.clone());
                }
            }
        }
    }
    let host_of = |id: u32| conns.iter().find(|c| c.0 == id).map(|c| c.1.clone()).unwrap_or_default();
    let mut rows: Vec<dock::Row> = people
        .into_iter()
        .map(|((conn, _), (nick, mut chans))| {
            chans.sort_by_key(|c| c.to_lowercase());
            dock::Row { cells: vec![nick.clone(), host_of(conn), chans.join(", ")], target: dock::Target::User { conn, nick } }
        })
        .collect();
    rows.sort_by_cached_key(|r| (r.cells[0].to_lowercase(), r.cells[1].clone()));
    rows.truncate(USER_ROWS_MAX);
    dock::set_rows(Pane::Users, rows);
}

/// A server window of connection `conn` (or the main window), for messages from the docked panes.
fn conn_window(conn: u32) -> HWND {
    with(|a| a.conns.get(&conn).map(|c| c.server_win)).filter(|h| *h != 0).map(hw).unwrap_or_else(ui::frame)
}

/// Users pane: add that person to the buddy list.
pub fn dock_add_buddy(conn: u32, nick: &str) {
    buddy_add(conn_window(conn), nick, "");
}

/// Users pane: ignore that person.
pub fn dock_ignore(conn: u32, nick: &str) {
    ignore_add(conn_window(conn), conn, nick);
}

/// Messages pane: open (or switch to) the chat with that person.
pub fn dock_open_query(conn: u32, nick: &str) {
    if let Some(h) = window_for_ex(conn, Kind::Query, nick, true, true) {
        ui::activate(hw(h));
    }
}

/// URLs pane: open an address in the browser. Only http and https are ever opened.
pub fn dock_open_url(url: &str) {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        shell_open(Path::new(url));
    }
}

/// Channels monitor: switch to the channel's window.
pub fn dock_goto_channel(conn: u32, name: &str) {
    if let Some(h) = window_for(conn, Kind::Channel, name, false) {
        ui::activate(hw(h));
    }
}

/// Connections monitor: switch to the server window.
pub fn dock_goto_connection(conn: u32) {
    if let Some(h) = with(|a| a.conns.get(&conn).map(|c| c.server_win)).filter(|h| *h != 0) {
        ui::activate(hw(h));
    }
}

/// Remembered column widths of a list (96-dpi pixels), when there are some for `columns` columns.
pub fn saved_columns(key: &str, columns: usize) -> Option<Vec<u32>> {
    with(|a| crate::columns::get(&a.settings.columns, key, columns).map(|w| w.to_vec()))
}

pub fn remember_columns(key: &str, widths: &[u32]) {
    with(|a| {
        if crate::columns::set(&mut a.settings.columns, key, widths) {
            a.settings.save();
        }
    });
}

/// Where a chat window of this kind and name was when it was last closed.
pub fn saved_child_window(kind: &str, name: &str) -> Option<crate::winsizes::Saved> {
    with(|a| crate::winsizes::find(&a.settings.child_windows, &crate::winsizes::key(kind, name)).cloned())
}

pub fn remember_child_window(s: crate::winsizes::Saved) {
    with(|a| {
        crate::winsizes::remember(&mut a.settings.child_windows, s);
        a.settings.save();
    });
}

/// What was last typed in the filter box of the list pane called `key` (its lower-case title).
pub fn saved_filter(key: &str) -> String {
    with(|a| a.settings.filters.iter().find(|(k, _)| k == key).map(|(_, t)| t.clone()).unwrap_or_default())
}

/// Remember what is typed in a pane's filter box (an empty box is forgotten).
pub fn remember_filter(key: &str, text: &str) {
    let text: String = text.trim().chars().take(100).collect();
    with(|a| {
        a.settings.filters.retain(|(k, _)| k != key);
        if !text.is_empty() && a.settings.filters.len() < 12 {
            a.settings.filters.push((key.to_owned(), text));
        }
        a.settings.save();
    });
}

/// The look as saved: theme, own chat colours, chat font, interface font.
pub fn look_settings() -> (crate::theme::Choice, crate::theme::Overrides, crate::theme::FontSpec, crate::theme::FontSpec) {
    with(|a| (a.settings.theme, a.settings.colors, a.settings.chat_font.clone(), a.settings.ui_font.clone()))
}

/// Options > Theme.
pub fn set_theme(choice: crate::theme::Choice) {
    with(|a| {
        a.settings.theme = choice;
        a.settings.save();
    });
    crate::look::set_choice(choice);
}

/// Options > Chat Colors: the colours of the chat text, as `#rrggbb` (empty keeps the theme's).
pub fn colors_dialog() {
    use crate::theme::{self, Overrides, COLOR_NAMES};
    let labels = ["Background", "Text", "Status lines (joins, modes)", "Actions (/me)", "Notices", "Errors", "Timestamps", "User list: channel owner (~)", "User list: admin (&)", "User list: operator (@)", "User list: half-operator (%)", "User list: voiced (+)"];
    let mut current = with(|a| a.settings.colors);
    loop {
        let mut fields: Vec<Field> = vec![Field::note("Colors of the chat windows and the user list as #rrggbb (for example #1e1e1e). Leave a box empty to use the theme's own color. Colors that come with the text (mIRC colors) are made readable on the background you choose.")];
        for (name, label) in COLOR_NAMES.iter().zip(labels) {
            fields.push(Field::text(label, &current.get(name).map(theme::hex).unwrap_or_default()));
        }
        fields.push(Field::check("Back to the theme's colors", false));
        let Some(v) = dialogs::show(ui::frame(), "Chat Colors", &fields) else { return };
        let mut next = Overrides::default();
        let mut bad: Option<String> = None;
        if v[COLOR_NAMES.len() + 1] != "1" {
            for (i, name) in COLOR_NAMES.iter().enumerate() {
                let text = v[i + 1].trim();
                if text.is_empty() {
                    continue;
                }
                match theme::parse_hex(text) {
                    Some(c) => {
                        next.set(name, Some(c));
                    }
                    None => bad = Some(format!("\"{text}\" is not a color. Use #rrggbb, for example #202020.")),
                }
            }
        }
        if let Some(e) = bad {
            // keep what was typed so far for the next round
            for (i, name) in COLOR_NAMES.iter().enumerate() {
                current.set(name, theme::parse_hex(v[i + 1].trim()));
            }
            dialogs::message(ui::frame(), "Chat Colors", &e);
            continue;
        }
        with(|a| {
            a.settings.colors = next;
            a.settings.save();
        });
        crate::look::set_overrides(next);
        return;
    }
}

/// Options > Chat Font / Interface Font.
pub fn font_dialog(chat: bool) {
    let current = if chat { crate::look::chat_font() } else { crate::look::ui_font() };
    let Some(picked) = crate::look::pick_font(ui::frame(), &current) else { return };
    let (chat_font, ui_font) = if chat { (picked, crate::look::ui_font()) } else { (crate::look::chat_font(), picked) };
    with(|a| {
        a.settings.chat_font = chat_font.clone();
        a.settings.ui_font = ui_font.clone();
        a.settings.save();
    });
    crate::look::set_fonts(chat_font, ui_font);
}

/// Where the main window was last closed, if that was ever recorded.
pub fn saved_window() -> Option<crate::winplace::Placement> {
    with(|a| a.settings.window)
}

/// Remember where the main window is (called as it closes).
pub fn remember_window(p: crate::winplace::Placement) {
    with(|a| {
        a.settings.window = Some(p);
        a.settings.save();
    });
}

/// The dock layout stored in the settings.
fn dock_layout() -> Layout {
    with(|a| a.settings.dock).normalized()
}

fn store_dock_layout(l: Layout) {
    with(|a| {
        a.settings.dock = l;
        a.settings.save();
    });
}

/// F9 / F8: bring a pane to the front (giving it a tab if it had none), or hide the dock when that
/// pane is already the one in front.
pub fn dock_toggle(pane: u8) {
    let mut l = dock_layout();
    let Some(p) = Pane::from_index(pane) else { return };
    l.toggle(p);
    store_dock_layout(l.normalized());
    sync_menu();
}

/// View > Panes: switch one pane's tab on or off. Switching a pane on brings it to the front;
/// the last remaining pane cannot be switched off.
pub fn dock_pane_switch(index: usize) {
    let Some(p) = Pane::from_index(index as u8) else { return };
    let mut l = dock_layout();
    if l.shows(p) {
        if !l.remove_pane(p) {
            return; // the last pane stays
        }
    } else {
        l.add_pane(p, None);
    }
    store_dock_layout(l.normalized());
    sync_menu();
}

/// View > Move All To: put every pane that has a tab into one dock on that side of the window.
pub fn dock_set_edge(index: usize) {
    let Some(edge) = layouts::Edge::from_index(index as u8) else { return };
    let mut l = dock_layout();
    l.gather(edge);
    store_dock_layout(l.normalized());
    sync_menu();
}

/// A tab was dropped away from the docks: give its pane a window of its own, its top left corner at
/// (`left`, `top`) on the screen.
pub fn dock_float_pane(pane: usize, left: i32, top: i32) {
    let Some(p) = Pane::from_index(pane as u8) else { return };
    let mut l = dock_layout();
    l.float_pane(p, left, top, crate::util::px(360), crate::util::px(300));
    store_dock_layout(l.normalized());
    sync_menu();
}

/// Tab menu: move a pane to the dock on another edge (made if there is none).
pub fn dock_move_pane(pane: usize, edge: usize) {
    dock_place_pane(pane, edge, 0, None);
}

/// Tab menu: a tab to the other half of its dock (splitting the dock when it has only one half).
pub fn dock_other_half(pane: usize) {
    let Some(p) = Pane::from_index(pane as u8) else { return };
    let mut l = dock_layout();
    l.other_half(p);
    store_dock_layout(l.normalized());
    sync_menu();
}

/// Tab menu: join the two halves of the dock on `edge`.
pub fn dock_merge(edge: usize) {
    let Some(e) = layouts::Edge::from_index(edge as u8) else { return };
    let mut l = dock_layout();
    l.merge(e);
    store_dock_layout(l.normalized());
    sync_menu();
}

/// A tab was dropped on a dock (or an edge): the pane goes into half `group` of it, at tab position
/// `index` when that is known (which also reorders the tabs of a dock half).
pub fn dock_place_pane(pane: usize, edge: usize, group: usize, index: Option<usize>) {
    let (Some(p), Some(e)) = (Pane::from_index(pane as u8), layouts::Edge::from_index(edge as u8)) else { return };
    let mut l = dock_layout();
    l.place_in(p, e, group, index);
    store_dock_layout(l.normalized());
    sync_menu();
}

/// View > Layouts > Save: remember the dock as it is now in one of the five slots.
pub fn layout_save(slot: usize) {
    if slot >= layouts::SLOTS {
        return;
    }
    let l = dock_layout();
    with(|a| {
        a.settings.layouts[slot] = Some(l);
        a.settings.save();
    });
    server_print_any(&format!("* Saved the dock layout in slot {} (Alt+{} loads it).", slot + 1, slot + 1));
}

/// View > Layouts > Load (or Alt+1..5): put a saved layout back.
pub fn layout_load(slot: usize) {
    let saved = with(|a| a.settings.layouts.get(slot).copied().flatten());
    match saved {
        Some(l) => {
            store_dock_layout(l.normalized());
            sync_menu();
        }
        None => dialogs::info(ui::frame(), "Layouts", &format!("Layout {} is empty. Arrange the dock, then use View > Layouts > Save Layout {}.", slot + 1, slot + 1)),
    }
}

/// A status line in the most recent server window, or just the Event Log when there is none.
fn server_print_any(line: &str) {
    let conn = ui::active_child().and_then(|h| ui::child(h).map(|c| c.conn));
    match conn {
        Some(c) => server_print(c, line),
        None => dock::event(line.trim_start_matches("* ")),
    }
}

/// The dock was resized by dragging its splitter.
pub fn dock_resized() {
    store_dock_layout(dock::current());
}

/// The pane in front was changed with the tabs.
pub fn dock_changed() {
    let l = dock::current();
    store_dock_layout(l);
    ui::apply_dock(l);
    refresh_monitors();
}

fn merged_servers() -> Vec<servers::Server> {
    with(|a| servers::merged(&a.settings.servers))
}

pub fn refresh_server_list() {
    dock::refresh_servers(&merged_servers());
}

/// True for a server the user added (or overrode), as opposed to a built-in one.
pub fn server_is_user(host: &str, port: u16) -> bool {
    with(|a| a.settings.servers.iter().any(|s| s.same_endpoint(host, port)))
}

/// A server in the list was chosen (double click / Enter / Connect): connect to it, or say that we
/// already are.
pub fn server_list_connect(s: &servers::Server) {
    let existing = with(|a| {
        a.conns
            .iter()
            .find(|(_, c)| c.client.is_some() && c.host.eq_ignore_ascii_case(&s.host) && c.port == s.port)
            .map(|(id, _)| *id)
    });
    if let Some(conn) = existing {
        server_print(conn, &format!("* Already connected to {}.", s.host));
        if let Some(h) = with(|a| a.conns.get(&conn).map(|c| c.server_win)).filter(|h| *h != 0) {
            ui::activate(hw(h));
        }
        return;
    }
    let Some(cfg) = connect_config(&s.host, s.port, s.tls, None) else { return };
    connect(connection_for_new_server(), cfg, Vec::new());
}

fn server_form(title: &str, network: &str, host: &str, port: u16, tls: bool) -> Option<servers::Server> {
    let owner = ui::frame();
    let mut port_text = if port == 0 { "6697".to_owned() } else { port.to_string() };
    let (mut network, mut host, mut tls) = (network.to_owned(), host.to_owned(), tls || port == 0);
    loop {
        let fields = vec![
            Field::text("Network", &network),
            Field::text("Server", &host),
            Field::number("Port", &port_text),
            Field::check("Use TLS (recommended)", tls),
        ];
        let v = dialogs::show(owner, title, &fields)?;
        network = v[0].clone();
        host = v[1].trim().to_owned();
        port_text = v[2].trim().to_owned();
        tls = v[3] == "1";
        match port_text.parse::<u16>().ok().and_then(|p| servers::Server::new(&network, &host, p, tls)) {
            Some(s) => return Some(s),
            None => dialogs::message(owner, title, "Enter a server name (no spaces) and a port from 1 to 65535."),
        }
    }
}

pub fn server_list_add(network: Option<&str>) {
    let Some(s) = server_form("Add Server", network.unwrap_or(""), "", 0, true) else { return };
    with(|a| {
        servers::add(&mut a.settings.servers, s);
        a.settings.save();
    });
    refresh_server_list();
}

pub fn server_list_edit(old: &servers::Server) {
    let Some(new) = server_form("Edit Server", &old.network, &old.host, old.port, old.tls) else { return };
    with(|a| {
        // Editing a built-in entry stores the edited copy as the user's own (it overrides the built-in one).
        servers::remove(&mut a.settings.servers, &old.host, old.port);
        servers::add(&mut a.settings.servers, new);
        a.settings.save();
    });
    refresh_server_list();
}

pub fn server_list_remove(s: &servers::Server) {
    let removed = with(|a| {
        let r = servers::remove(&mut a.settings.servers, &s.host, s.port);
        if r {
            a.settings.save();
        }
        r
    });
    if removed {
        refresh_server_list();
    }
}

// ------------------------------------------------------------------ favorites

/// Rebuild the dynamic part of the Favorites menu from the saved favorites.
pub fn sync_favorites() {
    let items: Vec<ui::FavMenuServer> = with(|a| {
        a.settings
            .favorites
            .servers
            .iter()
            .map(|s| ui::FavMenuServer { label: format!("{}  ({})", s.label(), s.host), channels: s.channels.clone() })
            .collect()
    });
    ui::set_favorites_menu(&items);
}

/// A Favorites menu item was chosen: `channel == None` connects to the server and joins all its
/// channels; `Some(i)` joins just that channel.
pub fn favorite_action(server: usize, channel: Option<usize>) {
    let Some(fav) = with(|a| a.settings.favorites.servers.get(server).cloned()) else { return };
    let chans: Vec<String> = match channel {
        Some(i) => fav.channels.get(i).cloned().into_iter().collect(),
        None => fav.channels.clone(),
    };
    // Already connected (or connecting) to this server? Join there instead of opening a second link.
    let existing = with(|a| {
        a.conns
            .iter()
            .find(|(_, c)| c.client.is_some() && c.host.eq_ignore_ascii_case(&fav.host) && c.port == fav.port)
            .map(|(id, c)| (*id, c.registered))
    });
    match existing {
        Some((conn, true)) => {
            for c in &chans {
                join_channel(conn, c);
            }
        }
        Some((conn, false)) => with(|a| {
            if let Some(c) = a.conns.get_mut(&conn) {
                c.autojoin.extend(chans);
            }
        }),
        None => {
            let Some(cfg) = connect_config(&fav.host, fav.port, fav.tls, None) else { return };
            connect(connection_for_new_server(), cfg, chans);
        }
    }
}

/// Menu command: add the active window (a channel, or a server with its open channels) to favorites.
pub fn add_to_favorites() {
    let Some(h) = ui::active_child() else { return };
    let Some((kind, conn, name)) = ui::child(h).map(|c| (c.kind, c.conn, c.name.clone())) else { return };
    let Some((host, port, tls)) = with(|a| a.conns.get(&conn).map(|c| (c.host.clone(), c.port, c.tls))) else { return };
    if host.is_empty() {
        local(h, "* Connect to a server first, then add it to your favorites.");
        return;
    }
    let msg = match kind {
        Kind::Query | Kind::DccChat => {
            local(h, "* Favorites hold servers and channels; use a channel or server window.");
            return;
        }
        Kind::Channel => {
            let added = with(|a| a.settings.favorites.add_channel(&host, port, tls, &name));
            match added {
                favorites::Added::ServerAndChannel => format!("* Added {name} and {host} to your favorites."),
                favorites::Added::Channel => format!("* Added {name} to your favorites for {host}."),
                favorites::Added::AlreadyThere => format!("* {name} is already in your favorites."),
            }
        }
        Kind::Server => {
            let open = channel_names(conn);
            let added = with(|a| a.settings.favorites.add_server(favorites::Server::new("", &host, port, tls, &open)));
            if added {
                format!("* Added {host} to your favorites with {} channel(s).", open.len())
            } else {
                // Already a favorite: still pick up any newly joined channels.
                for c in &open {
                    with(|a| a.settings.favorites.add_channel(&host, port, tls, c));
                }
                format!("* {host} is already in your favorites (channels updated).")
            }
        }
    };
    with(|a| a.settings.save());
    sync_favorites();
    local(h, &msg);
}

// ------------------------------------------------------------------ ignore list

/// The enabled ignore rules, as handed to connections.
fn ignore_rules() -> Vec<chimerairc_core::IgnoreRule> {
    with(|a| ignorelist::rules(&a.settings.ignores))
}

/// Give every live connection the current rules (new connections get them in their `Config`).
pub fn push_ignores() {
    let rules = ignore_rules();
    with(|a| {
        for c in a.conns.values() {
            if let Some(client) = &c.client {
                client.set_ignores(rules.clone());
            }
        }
    });
}

/// Is there a plain "ignore this nick" entry for `nick`? (Decides Ignore vs Unignore in the user menu.)
pub fn is_ignoring_nick(nick: &str) -> bool {
    with(|a| ignorelist::find_nick(&a.settings.ignores, nick).is_some())
}

fn save_ignores_changed() {
    with(|a| a.settings.save());
    push_ignores();
}

/// Start ignoring a nick or mask everywhere. Reports the outcome in window `h`.
fn ignore_add(h: HWND, conn: u32, who: &str) {
    let who = who.trim();
    if who.is_empty() || who.contains(char::is_whitespace) {
        local(h, "* Usage: /ignore <nick or mask>   (e.g. /ignore troll  or  /ignore *!*@*.example.net)");
        return;
    }
    if eq_ic(who, &nick_of(conn)) {
        local(h, "* You can't ignore yourself.");
        return;
    }
    let entry = ignorelist::Entry::new(who);
    let mask = entry.rule.mask.clone();
    if with(|a| ignorelist::add(&mut a.settings.ignores, entry)) {
        save_ignores_changed();
        local(h, &format!("* Now ignoring {mask}. Use /unignore {who} or Options > Ignore List to undo."));
    } else {
        local(h, &format!("* Already ignoring {mask}."));
    }
}

fn ignore_remove(h: HWND, who: &str) {
    let who = who.trim();
    if who.is_empty() {
        local(h, "* Usage: /unignore <nick or mask>");
        return;
    }
    let removed = with(|a| ignorelist::remove(&mut a.settings.ignores, who));
    if removed > 0 {
        save_ignores_changed();
        local(h, &format!("* No longer ignoring {who} ({removed} entr{}).", if removed == 1 { "y" } else { "ies" }));
    } else {
        local(h, &format!("* You aren't ignoring {who}."));
    }
}

fn ignore_list(h: HWND) {
    let entries = settings().ignores;
    if entries.is_empty() {
        local(h, "* Your ignore list is empty. Use /ignore <nick> or Options > Ignore List.");
        return;
    }
    local(h, &format!("* Ignore list ({} entr{}):", entries.len(), if entries.len() == 1 { "y" } else { "ies" }));
    for e in &entries {
        local(h, &format!("    {}", e.describe()));
    }
}

// ------------------------------------------------------------------ auto-perform

/// The network (as named in the server list) that a host belongs to.
fn network_of(host: &str) -> Option<String> {
    merged_servers().into_iter().find(|s| s.host.eq_ignore_ascii_case(host)).map(|s| s.network)
}

/// The auto-perform script that applies to a connection to `host`, if there is one.
fn perform_script(host: &str) -> Option<Vec<String>> {
    let network = network_of(host);
    with(|a| perform::find(&a.settings.perform, host, network.as_deref()).map(|p| p.lines.clone()))
}

/// Begin running a script on a registered connection: its commands in order (with pauses), then the channels.
fn start_perform(conn: u32, host: &str, lines: Vec<String>, chans: Vec<String>) {
    let generation = with(|a| {
        a.conns.get_mut(&conn).map(|c| {
            c.perform_gen = c.perform_gen.wrapping_add(1);
            c.perform_gen
        })
    });
    let Some(generation) = generation else { return };
    server_print(conn, &format!("* Auto-perform for {host}: {} command(s){}.", lines.len(), if chans.is_empty() { "" } else { ", then joining the favorite's channels" }));
    let mut steps: Vec<PerformStep> = lines.into_iter().map(PerformStep::Line).collect();
    steps.extend(chans.chunks(8).map(|g| PerformStep::Join(g.to_vec())));
    PERFORM.lock().unwrap().push_back((conn, generation, steps));
    wake_ui();
}

/// Run the next steps of a script, up to a pause (the rest is queued again when the pause is over).
fn run_perform(conn: u32, generation: u32, mut steps: Vec<PerformStep>) {
    while !steps.is_empty() {
        let live = with(|a| a.conns.get(&conn).is_some_and(|c| c.client.is_some() && c.registered && c.perform_gen == generation));
        if !live {
            return; // disconnected, or the script was started again meanwhile
        }
        match steps.remove(0) {
            PerformStep::Join(chans) => {
                send(conn, Message::new("JOIN", &[&chans.join(",")]));
            }
            PerformStep::Line(line) => {
                if let Some(secs) = perform::wait_secs(&line) {
                    server_print(conn, &format!("* Auto-perform: waiting {secs} s."));
                    with(|a| {
                        a.rt.spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(secs as u64)).await;
                            PERFORM.lock().unwrap().push_back((conn, generation, steps));
                            wake_ui();
                        })
                    });
                    return;
                }
                perform_line(conn, &line);
            }
        }
    }
}

/// Run one command of a script. A value asked for (a password) is used only for this line: it is never
/// shown, logged or stored, which is why such a line is sent directly instead of through the command
/// handler (which would echo what is sent).
fn perform_line(conn: u32, line: &str) {
    let line = &perform::with_implied_ask(line);
    let Some((nick, host, win)) = with(|a| a.conns.get(&conn).map(|c| (c.nick.clone(), c.host.clone(), c.server_win))) else { return };
    let title = format!("Auto-perform on {host}");
    let mut ask = |label: &str| -> Option<String> {
        let label = if label.is_empty() { "Value" } else { label };
        let v = dialogs::show(ui::frame(), &title, &[Field::password(label)])?;
        // an empty answer counts as giving up: a half-finished command is worse than none
        (!v[0].is_empty()).then(|| v[0].clone())
    };
    let Some((text, asked)) = perform::expand(line, &nick, &host, &mut ask) else {
        server_print(conn, &format!("* Auto-perform: skipped (not answered): {}", perform::masked(line)));
        return;
    };
    server_print(conn, &format!("* Auto-perform: {}", perform::masked(line)));
    let cmd = text.strip_prefix('/').unwrap_or(&text);
    if asked {
        if !send_unechoed(conn, cmd) {
            server_print(conn, "* Auto-perform: a line that asks for a password can only be /msg, /notice, /ns, /cs, /oper, /pass or /raw; it was not sent.");
        }
    } else if win != 0 {
        // messages to services are sent from the server window: they must not open (and focus) a chat window
        let (verb, rest) = cmd.split_once(' ').unwrap_or((cmd, ""));
        let rest = rest.trim();
        let message = match verb.to_ascii_lowercase().as_str() {
            "msg" | "privmsg" => rest.split_once(' ').map(|(t, x)| ("PRIVMSG", t, x.trim())),
            "notice" => rest.split_once(' ').map(|(t, x)| ("NOTICE", t, x.trim())),
            "ns" | "nickserv" if !rest.is_empty() => Some(("PRIVMSG", "NickServ", rest)),
            "cs" | "chanserv" if !rest.is_empty() => Some(("PRIVMSG", "ChanServ", rest)),
            _ => None,
        };
        match message {
            Some((how, target, text)) if !text.is_empty() && !is_channel(target) => send_chat(hw(win), conn, target, how, text),
            _ => command(hw(win), Kind::Server, conn, &host, cmd),
        }
    }
}

thread_local! {
    /// Messages sent with a secret in them, by (connection, lower-case target, text): when the server echoes
    /// one back (the `echo-message` capability) it must not be shown, logged or fed to any pane.
    static SECRET_ECHOES: RefCell<Vec<(u32, String, String, std::time::Instant)>> = const { RefCell::new(Vec::new()) };
}

/// Remember that a message with a secret in it was just sent, so that its echo can be dropped.
fn expect_secret_echo(conn: u32, target: &str, text: &str) {
    SECRET_ECHOES.with(|e| {
        let mut e = e.borrow_mut();
        e.retain(|x| x.3.elapsed().as_secs() < 60);
        e.push((conn, target.to_lowercase(), text.to_owned(), std::time::Instant::now()));
    });
}

/// Is this incoming message the echo of one sent with a secret? (Then it is forgotten and must be ignored.)
fn is_secret_echo(conn: u32, target: &str, text: &str) -> bool {
    SECRET_ECHOES.with(|e| {
        let mut e = e.borrow_mut();
        match e.iter().position(|x| x.0 == conn && x.1 == target.to_lowercase() && x.2 == text && x.3.elapsed().as_secs() < 60) {
            Some(i) => {
                e.remove(i);
                true
            }
            None => false,
        }
    })
}

/// Send a command that contains a secret without printing or logging it. False when this kind of command
/// cannot be sent that way.
fn send_unechoed(conn: u32, cmd: &str) -> bool {
    let (verb, rest) = cmd.split_once(' ').unwrap_or((cmd, ""));
    let rest = rest.trim();
    let verb = verb.to_ascii_lowercase();
    match verb.as_str() {
        "msg" | "privmsg" | "notice" | "squery" => match rest.split_once(' ') {
            Some((target, text)) if !text.trim().is_empty() => {
                let command = if verb == "notice" { "NOTICE" } else if verb == "squery" { "SQUERY" } else { "PRIVMSG" };
                expect_secret_echo(conn, target, text.trim());
                send(conn, Message::new(command, &[target, text.trim()]));
                true
            }
            _ => false,
        },
        "ns" | "nickserv" if !rest.is_empty() => {
            expect_secret_echo(conn, "NickServ", rest);
            send(conn, Message::new("PRIVMSG", &["NickServ", rest]));
            true
        }
        "cs" | "chanserv" if !rest.is_empty() => {
            expect_secret_echo(conn, "ChanServ", rest);
            send(conn, Message::new("PRIVMSG", &["ChanServ", rest]));
            true
        }
        "oper" | "pass" | "password" if !rest.is_empty() => {
            let name = if verb == "oper" { "OPER" } else { "PASS" };
            let params: Vec<&str> = rest.split_whitespace().collect();
            send(conn, Message::new(name, &params));
            true
        }
        "raw" | "quote" if !rest.is_empty() => match Message::parse(rest) {
            Ok(m) => {
                send(conn, m);
                true
            }
            Err(_) => false,
        },
        _ => false,
    }
}

/// `/perform list`: print every profile (passwords cannot be in them).
fn perform_list(h: HWND) {
    let profiles = settings().perform;
    if profiles.is_empty() {
        local(h, "* No auto-perform scripts. Use /perform edit or Options > Auto-Perform.");
        return;
    }
    for p in &profiles {
        local(h, &format!("* Auto-perform for {}:", p.network));
        for l in &p.lines {
            local(h, &format!("    {}", perform::masked(l)));
        }
    }
}

/// Options > Auto-Perform: edit the script of a network (the one of the active connection when `network` is
/// not given). An empty script, or ticking the box, deletes the profile.
/// The identity that applies to a connection to `host` (its own, else its network's).
fn identity_for(host: &str) -> Option<identity::Identity> {
    let network = network_of(host);
    with(|a| identity::find(&a.settings.identities, host, network.as_deref()).cloned())
}

/// The configuration for a connection the user asked for: the identity of the server or network applied
/// (nickname, alternates, user name, real name, SASL account), and the passwords it needs asked for now.
/// `explicit_nick` (typed in a command) wins over the identity's. `None` when the user cancels.
fn connect_config(host: &str, port: u16, tls: bool, explicit_nick: Option<&str>) -> Option<Config> {
    let ident = identity_for(host);
    let nick = explicit_nick
        .map(str::to_owned)
        .or_else(|| ident.as_ref().map(|i| i.nick.clone()).filter(|n| !n.is_empty()))
        .unwrap_or_else(|| settings().nick);
    let mut cfg = base_config(host, port, tls, &nick);
    let Some(i) = ident else { return Some(cfg) };
    if !i.user.is_empty() {
        cfg.user = i.user.clone();
    }
    if !i.realname.is_empty() {
        cfg.realname = i.realname.clone();
    }
    cfg.alt_nicks = i.alt_nicks.clone();
    let wants_sasl = !i.sasl_account.is_empty();
    if !wants_sasl && !i.ask_password {
        return Some(cfg);
    }
    let owner = ui::frame();
    if !tls
        && !dialogs::confirm(
            owner,
            "Unencrypted connection",
            "TLS is off for this server, so a password would be readable by anyone on the network (a SASL login uses SCRAM-SHA-256 when the server offers it, otherwise PLAIN, which sends the password in clear text).\n\nAsk for the password and send it anyway?",
        )
    {
        return None;
    }
    let mut fields = vec![Field::note("Passwords are used for this connection only and are never saved.")];
    let sasl_at = wants_sasl.then(|| {
        fields.push(Field::password(&format!("SASL password for {}", i.sasl_account)));
        fields.len() - 1
    });
    let pass_at = i.ask_password.then(|| {
        fields.push(Field::password("Server password"));
        fields.len() - 1
    });
    loop {
        let v = dialogs::show(owner, &format!("Passwords for {host}"), &fields)?;
        if let Some(at) = sasl_at {
            if v[at].is_empty() {
                dialogs::message(owner, "Passwords", "Enter the SASL password, or press Cancel to stop connecting.");
                continue;
            }
            cfg.sasl = Some((i.sasl_account.clone(), v[at].clone()));
        }
        if let Some(at) = pass_at {
            if !v[at].is_empty() {
                cfg.password = Some(v[at].clone());
            }
        }
        return Some(cfg);
    }
}

thread_local! {
    /// How many aliases are running inside each other right now.
    static ALIAS_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The names of the user's aliases (for tab completion).
pub fn alias_names() -> Vec<String> {
    with(|a| a.settings.aliases.iter().map(|x| x.name.clone()).collect())
}

/// Run the alias called `verb` with the words typed after it. False when there is no such alias.
fn run_alias(h: HWND, verb: &str, rest: &str) -> bool {
    let Some(a) = with(|s| alias::find(&s.settings.aliases, verb).cloned()) else { return false };
    let depth = ALIAS_DEPTH.with(|d| d.get());
    if depth >= alias::MAX_DEPTH {
        local(h, &format!("* /{} stopped: aliases are nested too deeply.", a.name));
        return true;
    }
    let Some((kind, conn, name)) = ui::child(h).map(|c| (c.kind, c.conn, c.name.clone())) else { return true };
    let nick = nick_of(conn);
    let server = with(|s| s.conns.get(&conn).map(|c| c.host.clone())).unwrap_or_default();
    let chan = if matches!(kind, Kind::Channel | Kind::Query) { name } else { String::new() };
    let uptime = alias::duration_text(crate::util::uptime_secs());
    ALIAS_DEPTH.with(|d| d.set(depth + 1));
    for cmd in a.commands() {
        let line = alias::expand(&cmd, rest, &alias::Vars { nick: &nick, chan: &chan, server: &server, uptime: &uptime });
        if !line.trim().is_empty() {
            submit(h, line);
        }
    }
    ALIAS_DEPTH.with(|d| d.set(depth));
    true
}

/// `/alias` lists them, `/alias name text` adds or replaces one, `/alias -d name` removes one.
fn alias_command(h: HWND, rest: &str) {
    if rest.is_empty() {
        let list = settings().aliases;
        if list.is_empty() {
            local(h, "* You have no aliases. Make one with /alias slap /me slaps $1 around with a large trout  (or Options > Aliases...).");
        } else {
            local(h, &format!("* Aliases ({}):", list.len()));
            for a in &list {
                local(h, &format!("    /{} = {}", a.name, a.text));
            }
        }
        return;
    }
    if let Some(name) = rest.strip_prefix("-d ").or_else(|| rest.strip_prefix("-r ")) {
        if with(|a| alias::remove(&mut a.settings.aliases, name)) {
            with(|a| a.settings.save());
            local(h, &format!("* Removed /{}.", name.trim().trim_start_matches('/')));
        } else {
            local(h, &format!("* There is no alias /{}.", name.trim().trim_start_matches('/')));
        }
        return;
    }
    let (name, text) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    match alias::build(name, text).and_then(|a| with(|s| alias::set(&mut s.settings.aliases, a.clone()).map(|_| a))) {
        Ok(a) => {
            with(|s| s.settings.save());
            local(h, &format!("* /{} is now: {}", a.name, a.text));
        }
        Err(e) => local(h, &format!("* {e}")),
    }
}

/// Options > Aliases...: the user's own commands, one per line.
pub fn alias_dialog() {
    let owner = ui::frame();
    let mut text = alias::editor_text(&settings().aliases);
    const HELP: &str = "One alias per line: name = commands, with ' | ' between commands. In the text, $1 to $9 are the words you type after it, $1- all from the first, $* everything, $nick your nick, $chan this window's channel or person, $server and $uptime. \
Example:  slap = /me slaps $1 around with a large trout   Built-in commands cannot be replaced, and an alias cannot hold a password. Lines starting with ; are comments.";
    loop {
        let fields = [Field::note(HELP), Field::multiline("Aliases", &text)];
        let Some(v) = dialogs::show(owner, "Aliases", &fields) else { return };
        text = v[1].clone();
        match alias::parse_editor(&text) {
            Ok(list) => {
                with(|a| {
                    a.settings.aliases = list;
                    a.settings.save();
                });
                return;
            }
            Err(e) => dialogs::message(owner, "Aliases", &e),
        }
    }
}

/// Options > Identities...: the nickname, alternates, user name, real name and SASL account for a network or server.
pub fn identity_dialog(network: Option<&str>) {
    let active_host = ui::active_child()
        .and_then(|h| ui::child(h).map(|c| c.conn))
        .and_then(|c| with(|a| a.conns.get(&c).map(|c| c.host.clone())))
        .filter(|h| !h.is_empty());
    let mut name = network
        .map(str::to_owned)
        .or_else(|| active_host.as_deref().map(|h| network_of(h).unwrap_or_else(|| h.to_owned())))
        .unwrap_or_default();
    let owner = ui::frame();
    let mut cur = with(|a| a.settings.identities.iter().find(|i| i.network.eq_ignore_ascii_case(&name)).cloned()).unwrap_or_default();
    const HELP: &str = "For one network (its name in the server list, such as Libera.Chat) or one server (its host name). What you leave empty comes from Preferences. \
Alternate nicknames are tried in order when the nickname is taken. Passwords are never saved: with a SASL account the password is asked for each time you connect, \
and so is a server password if you tick the box.";
    loop {
        let fields = [
            Field::note(HELP),
            Field::text("Network or server", &name),
            Field::text("Nickname", &cur.nick),
            Field::text("Alternate nicknames", &cur.alt_nicks.join(", ")),
            Field::text("Username", &cur.user),
            Field::text("Real name", &cur.realname),
            Field::text("SASL account", &cur.sasl_account),
            Field::check("Ask for a server password when connecting", cur.ask_password),
            Field::check("Delete this identity", false),
        ];
        let Some(v) = dialogs::show(owner, "Identities", &fields) else { return };
        name = v[1].trim().to_owned();
        if v[8] == "1" {
            let removed = with(|a| identity::remove(&mut a.settings.identities, &name));
            if removed {
                with(|a| a.settings.save());
            }
            return;
        }
        match identity::build(&name, &v[2], &v[3], &v[4], &v[5], &v[6], v[7] == "1") {
            Ok(id) => {
                cur = id.clone();
                match with(|a| identity::set(&mut a.settings.identities, id)) {
                    Ok(()) => {
                        with(|a| a.settings.save());
                        return;
                    }
                    Err(e) => dialogs::message(owner, "Identities", &e),
                }
            }
            Err(e) => {
                dialogs::message(owner, "Identities", &e);
                // keep what was typed
                cur = Identity { network: name.clone(), nick: v[2].clone(), alt_nicks: Vec::new(), user: v[4].clone(), realname: v[5].clone(), sasl_account: v[6].clone(), ask_password: v[7] == "1" };
                cur.alt_nicks = v[3].split([',', ' ']).filter(|s| !s.is_empty()).map(str::to_owned).collect();
            }
        }
    }
}

thread_local! {
    /// What Find looked for last (text, match case), for F3.
    static LAST_FIND: std::cell::RefCell<(String, bool)> = const { std::cell::RefCell::new((String::new(), false)) };
}

/// Edit > Find...
pub fn find_dialog() {
    let (text, case) = LAST_FIND.with(|l| l.borrow().clone());
    let fields = [Field::text("Find", &text), Field::check("Match case", case)];
    let Some(v) = dialogs::show(ui::frame(), "Find in this window", &fields) else { return };
    let text = v[0].clone();
    if text.is_empty() {
        return;
    }
    LAST_FIND.with(|l| *l.borrow_mut() = (text, v[1] == "1"));
    find_again(true);
}

/// F3 / Shift+F3: the next or previous match of the last search (asking for one when there is none yet).
pub fn find_again(forward: bool) {
    let (text, case) = LAST_FIND.with(|l| l.borrow().clone());
    if text.is_empty() {
        find_dialog();
        return;
    }
    if !ui::find_in_output(&text, case, forward) {
        unsafe {
            let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(windows::Win32::UI::WindowsAndMessaging::MB_ICONASTERISK);
        }
        ui::set_status(0, &format!("\"{text}\" was not found in this window"));
    }
}

/// Edit > Search Logs...
pub fn log_search_dialog() {
    let fields = [
        Field::note("Searches the saved conversation logs (turn logging on in Options if there are none). At most 300 lines are shown, the newest files first."),
        Field::text("Search for (2 characters or more)", ""),
        Field::check("Only this server's logs", false),
        Field::check("Match case", false),
    ];
    let Some(v) = dialogs::show(ui::frame(), "Search Logs", &fields) else { return };
    run_log_search(v[1].trim(), v[2] == "1", v[3] == "1");
}

/// Search the logs and list what was found in the active window (without logging the list itself).
fn run_log_search(query: &str, this_server: bool, match_case: bool) {
    let Some(h) = ui::active_child() else { return };
    let show = |line: &str, kind: LineKind| ui::print(h, None, line, kind, &highlight::Marks::default());
    if query.chars().count() < logsearch::MIN_QUERY {
        show(&format!("* Enter at least {} characters to search the logs for.", logsearch::MIN_QUERY), LineKind::Info);
        return;
    }
    let base = log_base(&settings().log_dir);
    let host = if this_server {
        ui::child(h).and_then(|c| with(|a| a.conns.get(&c.conn).map(|c| c.host.clone()))).filter(|h| !h.is_empty())
    } else {
        None
    };
    let opt = logsearch::Options { case_sensitive: match_case, ..logsearch::Options::default() };
    let r = logsearch::search(&base, host.as_deref(), query, &opt);
    show(&format!("* --- Log search for \"{query}\": {} line{} in {} log{} ---", r.hits.len(), if r.hits.len() == 1 { "" } else { "s" }, r.files_searched, if r.files_searched == 1 { "" } else { "s" }), LineKind::Info);
    for hit in &r.hits {
        show(&logsearch::hit_text(hit), LineKind::Normal);
    }
    if r.truncated {
        show("* (The search stopped early: narrow it with more text or \"Only this server's logs\".)", LineKind::Info);
    }
    show("* --- end of log search ---", LineKind::Info);
}

pub fn perform_dialog(network: Option<&str>) {
    let st = settings();
    let active_host = ui::active_child().and_then(|h| ui::child(h).map(|c| c.conn)).and_then(|c| with(|a| a.conns.get(&c).map(|c| c.host.clone()))).filter(|h| !h.is_empty());
    let mut name = network
        .map(str::to_owned)
        .or_else(|| active_host.as_deref().map(|h| network_of(h).unwrap_or_else(|| h.to_owned())))
        .unwrap_or_default();
    let existing = |n: &str| st.perform.iter().find(|p| p.network.eq_ignore_ascii_case(n)).map(|p| p.lines.join("\n")).unwrap_or_default();
    let mut script = existing(&name);
    const HELP: &str = "One command per line, as you would type it, for example /join #rust or /mode {nick} +i. {nick} and {server} are filled in. \
{ask:Label} asks for a value each time (use it for passwords: /msg NickServ identify {ask:NickServ password}); passwords are never saved. \
/wait 2 pauses for 2 seconds. Lines starting with ; are comments. The script runs when the connection is made, before the favorite's channels are joined.";
    loop {
        let fields = [
            Field::note(HELP),
            Field::text("Network or server", &name),
            Field::multiline("Commands", &script),
            Field::check("Delete this profile", false),
        ];
        let Some(v) = dialogs::show(ui::frame(), "Auto-Perform", &fields) else { return };
        name = v[1].trim().to_owned();
        script = v[2].clone();
        let delete = v[3] == "1";
        let result = with(|a| perform::set(&mut a.settings.perform, &name, if delete { "" } else { &script }));
        match result {
            Ok(()) => {
                with(|a| a.settings.save());
                return;
            }
            Err(e) => dialogs::message(ui::frame(), "Auto-Perform", &e),
        }
    }
}

/// Is `nick` on the buddy list?
pub fn is_buddy(nick: &str) -> bool {
    with(|a| buddies::find(&a.settings.buddies, nick).is_some())
}

/// The buddy list changed: save it, tell every server and redraw the pane.
fn buddies_changed() {
    with(|a| {
        a.settings.save();
        let watch = buddies::nicks(&a.settings.buddies);
        for c in a.conns.values() {
            if let Some(cl) = &c.client {
                cl.watch(watch.clone());
            }
        }
    });
    refresh_buddies();
}

fn buddy_add(h: HWND, nick: &str, group: &str) {
    let nick = nick.trim();
    if nick.is_empty() {
        local(h, "* Usage: /buddy add <nick> [group]");
        return;
    }
    match with(|a| buddies::add(&mut a.settings.buddies, nick, group, "")) {
        Ok(()) => {
            buddies_changed();
            local(h, &format!("* {nick} is now on your buddy list."));
        }
        Err(e) => local(h, &format!("* {nick}: {}.", e.text())),
    }
}

fn buddy_remove(h: HWND, nick: &str) {
    let nick = nick.trim();
    if nick.is_empty() {
        local(h, "* Usage: /buddy remove <nick>");
        return;
    }
    if with(|a| buddies::remove(&mut a.settings.buddies, nick)) {
        buddies_changed();
        local(h, &format!("* {nick} is no longer on your buddy list."));
    } else {
        local(h, &format!("* {nick} is not on your buddy list."));
    }
}

fn buddy_list(h: HWND) {
    let list = settings().buddies;
    if list.is_empty() {
        local(h, "* Your buddy list is empty. Use /buddy add <nick>, or right-click a name in a channel.");
        return;
    }
    local(h, &format!("* Buddies ({}):", list.len()));
    for b in buddies::display_order(&list, &|n| buddy_online(n).is_some()) {
        let state = match buddy_online(&b.nick) {
            Some((_, host)) => format!("online on {host}"),
            None => "offline".to_owned(),
        };
        let group = if b.group.is_empty() { String::new() } else { format!(" [{}]", b.group) };
        local(h, &format!("    {}{group}: {state}", b.nick));
    }
}

/// The first connection `nick` is known to be online on: (connection id, server host).
fn buddy_online(nick: &str) -> Option<(u32, String)> {
    let key = nick.to_ascii_lowercase();
    with(|a| {
        let mut ids: Vec<u32> = a.conns.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter().find_map(|id| {
            let c = &a.conns[&id];
            c.presence.get(&key).filter(|(on, _)| *on).map(|_| (id, c.host.clone()))
        })
    })
}

/// Rebuild the Buddies pane: with groups in use, a heading for each (that can be folded up) and its buddies.
pub fn refresh_buddies() {
    let (list, collapsed) = with(|a| (a.settings.buddies.clone(), a.settings.collapsed_groups.clone()));
    let rows: Vec<dock::Row> = buddies::lines(&list, &|n| buddy_online(n).is_some(), &collapsed)
        .into_iter()
        .map(|line| match line {
            buddies::Line::Header { key, title, online, total, collapsed } => dock::Row {
                cells: vec![format!("{} {title}", if collapsed { "\u{25b6}" } else { "\u{25bc}" }), format!("{online}/{total}"), String::new(), String::new()],
                target: dock::Target::BuddyGroup { key },
            },
            buddies::Line::Buddy(b) => {
                let online = buddy_online(&b.nick);
                let (status, detail) = match &online {
                    Some((_, host)) => ("Online", host.clone()),
                    None => ("Offline", b.note.clone()),
                };
                dock::Row {
                    cells: vec![b.nick.clone(), status.to_owned(), b.group.clone(), detail],
                    target: dock::Target::Buddy { nick: b.nick.clone(), conn: online.map(|(id, _)| id) },
                }
            }
        })
        .collect();
    dock::set_rows(Pane::Buddies, rows);
}

/// Buddies pane: fold a group up or unfold it.
pub fn buddy_group_toggle(key: &str) {
    with(|a| {
        buddies::toggle_collapsed(&mut a.settings.collapsed_groups, key);
        a.settings.save();
    });
    refresh_buddies();
}

/// Buddies pane: fold every group up, or unfold them all.
pub fn buddy_groups_fold_all(fold: bool) {
    with(|a| {
        a.settings.collapsed_groups = if fold { buddies::all_group_keys(&a.settings.buddies) } else { Vec::new() };
        a.settings.save();
    });
    refresh_buddies();
}

/// Buddies pane: ask for a nick (and group) to add.
pub fn buddy_add_dialog() {
    let Some(v) = dialogs::show(ui::frame(), "Add Buddy", &[dialogs::Field::text("Nickname", ""), dialogs::Field::text("Group (optional)", "")]) else {
        return;
    };
    let (nick, group) = (v[0].trim().to_owned(), v[1].clone());
    if nick.is_empty() {
        return;
    }
    match with(|a| buddies::add(&mut a.settings.buddies, &nick, &group, "")) {
        Ok(()) => buddies_changed(),
        Err(e) => dialogs::message(ui::frame(), "Add Buddy", &format!("{nick}: {}.", e.text())),
    }
}

pub fn buddy_remove_from_dock(nick: &str) {
    if with(|a| buddies::remove(&mut a.settings.buddies, nick)) {
        buddies_changed();
    }
}

/// Buddies pane: WHOIS on the connection the buddy is online on.
pub fn dock_whois(conn: u32, nick: &str) {
    send(conn, Message::new("WHOIS", &[nick]));
}

/// Menu command: edit the ignore list.
pub fn ignore_dialog() {
    let current = settings().ignores;
    if let Some(new) = igndialog::show(ui::frame(), &current) {
        with(|a| a.settings.ignores = new);
        save_ignores_changed();
    }
}

pub fn organize_favorites() {
    let current = settings().favorites;
    if let Some(new) = favdialog::show(ui::frame(), &current) {
        with(|a| {
            a.settings.favorites = new;
            a.settings.save();
        });
        sync_favorites();
    }
}

// ------------------------------------------------------------------ channel list

/// Menu command: open the Channel List for the active window's connection.
pub fn channel_list_command() {
    let Some(conn) = ui::active_child().and_then(|h| ui::child(h).map(|c| c.conn)) else { return };
    open_channel_list(conn, None);
}

fn chanlist_window(conn: u32) -> Option<HWND> {
    with(|a| a.conns.get(&conn).map(|c| c.chanlist)).filter(|h| *h != 0).map(hw)
}

/// Show the Channel List for `conn`, fetching the list if it is not already there.
pub fn open_channel_list(conn: u32, search: Option<&str>) {
    if !with(|a| a.conns.get(&conn).is_some_and(|c| c.registered)) {
        dialogs::message(ui::frame(), "Channel List", "Connect to a server first (File > Connect).");
        return;
    }
    if let Some(h) = chanlist_window(conn) {
        chanwin::raise(h);
        if let Some(s) = search {
            chanwin::set_search(h, s);
        }
        return;
    }
    let host = with(|a| a.conns.get(&conn).map(|c| c.host.clone())).unwrap_or_default();
    let h = chanwin::create(ui::frame(), conn, &host);
    if h.0.is_null() {
        return;
    }
    with(|a| {
        if let Some(c) = a.conns.get_mut(&conn) {
            c.chanlist = raw(h);
        }
    });
    if let Some(s) = search {
        chanwin::set_search(h, s);
    }
    request_channel_list(conn);
}

/// Ask the server for its channel list (also used by the window's Refresh button).
pub fn request_channel_list(conn: u32) {
    if !send(conn, Message::new("LIST", &[])) {
        if let Some(h) = chanlist_window(conn) {
            chanwin::finish(h); // nothing is coming; leave the window usable
        }
        server_print(conn, "* Not connected.");
    }
}

pub fn channel_list_closed(conn: u32, h: isize) {
    with(|a| {
        if let Some(c) = a.conns.get_mut(&conn) {
            if c.chanlist == h {
                c.chanlist = 0;
            }
        }
    });
}

thread_local! {
    /// Pasted lines still to send, one every tick: (chat window, line).
    static PASTE_QUEUE: RefCell<std::collections::VecDeque<(isize, String)>> = const { RefCell::new(std::collections::VecDeque::new()) };
}

/// The entry box was asked to paste (Ctrl+V, Shift+Insert or Edit > Paste). Several lines are asked about first:
/// they would otherwise go out as separate messages without a word, or none of the lines would. Returns true when
/// the program dealt with the paste and the box must not paste by itself.
pub fn paste_requested(h: HWND) -> bool {
    let Some((kind, conn, name)) = ui::child(h).map(|c| (c.kind, c.conn, c.name.clone())) else { return false };
    if !matches!(kind, Kind::Channel | Kind::Query) {
        return false;
    }
    match ui::clipboard_content() {
        ui::Clip::Text(text) => {
            let lines = paste::split_lines(&text);
            if !paste::needs_question(&lines) {
                return false;
            }
            let owner = ui::frame();
            if lines.len() > paste::MAX_LINES {
                dialogs::message(
                    owner,
                    "Paste several lines",
                    &format!("That is {} lines; at most {} can be sent at once, so nothing was pasted. For a long text, use a paste site and share the link.", lines.len(), paste::MAX_LINES),
                );
                return true;
            }
            match dialogs::ask3(owner, "Paste several lines", &paste::question(&lines, &name)) {
                Some(true) => start_paste(h, kind, conn, &name, lines),
                Some(false) => ui::input_insert(h, &paste::joined(&lines)),
                None => {}
            }
            true
        }
        ui::Clip::Picture(dib) => {
            paste_picture(h, kind, conn, &name, &dib);
            true
        }
        ui::Clip::Nothing => false,
    }
}

/// Send the first line now and the rest one per tick of the paste timer.
fn start_paste(h: HWND, kind: Kind, conn: u32, name: &str, lines: Vec<String>) {
    let mut rest = lines.into_iter();
    if let Some(first) = rest.next() {
        say(h, kind, conn, name, &first);
    }
    let queued: Vec<(isize, String)> = rest.map(|l| (raw(h), l)).collect();
    if !queued.is_empty() {
        ui::set_status(0, &format!("Sending {} pasted lines to {name} (/stoppaste stops)...", queued.len()));
        PASTE_QUEUE.with(|q| q.borrow_mut().extend(queued));
    }
}

/// Once per paste timer tick: the next pasted line.
pub fn paste_tick() {
    let next = PASTE_QUEUE.with(|q| q.borrow_mut().pop_front());
    let Some((win, line)) = next else { return };
    match ui::child(hw(win)).map(|c| (c.kind, c.conn, c.name.clone())) {
        Some((kind, conn, name)) => say(hw(win), kind, conn, &name, &line),
        None => PASTE_QUEUE.with(|q| q.borrow_mut().retain(|(w, _)| *w != win)), // the window is gone: its lines too
    }
    if PASTE_QUEUE.with(|q| q.borrow().is_empty()) {
        refresh_status();
    } else {
        let left = PASTE_QUEUE.with(|q| q.borrow().len());
        ui::set_status(0, &format!("Sending pasted lines: {left} to go (/stoppaste stops)..."));
    }
}

/// `/stoppaste`: what has not been sent yet is dropped.
fn stop_paste(h: HWND) {
    let n = PASTE_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()).len());
    refresh_status();
    local(h, &if n == 0 { "* No pasted lines are waiting.".to_owned() } else { format!("* Stopped: {n} pasted line{} not sent.", if n == 1 { "" } else { "s" }) });
}

/// A picture was pasted: in a private chat it can be sent to that person as a file (a bitmap), and nothing is
/// uploaded anywhere.
fn paste_picture(h: HWND, kind: Kind, conn: u32, name: &str, dib: &[u8]) {
    let owner = ui::frame();
    if kind != Kind::Query {
        dialogs::info(owner, "Paste a picture", "A pasted picture is sent to one person as a file. Open a chat with them (double-click a name) and paste it there.");
        return;
    }
    let Some(bmp) = paste::bmp_from_dib(dib) else {
        dialogs::message(owner, "Paste a picture", "That picture could not be used.");
        return;
    };
    if !dialogs::confirm(owner, "Send picture", &format!("Send the picture on the clipboard to {name} as a bitmap file ({}) over DCC?", human_size(bmp.len() as u64))) {
        return;
    }
    let dir = std::env::temp_dir().join("ChimeraIRC");
    let stamp: String = datetime_stamp().chars().filter(|c| c.is_ascii_digit()).collect();
    let path = dir.join(format!("pasted-{stamp}.bmp"));
    match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &bmp)) {
        Ok(()) => dcc::send_file(h, conn, name, path),
        Err(e) => local(h, &format!("* The picture could not be saved to send it: {e}.")),
    }
}

/// Files were dropped on a chat window (at the screen point): a private chat offers each to that person, a channel
/// window offers them to the person whose name they were dropped on. Always asks first.
pub fn files_dropped(h: HWND, paths: Vec<PathBuf>, count: usize, sx: i32, sy: i32) {
    let owner = ui::frame();
    let Some((kind, conn, name)) = ui::child(h).map(|c| (c.kind, c.conn, c.name.clone())) else { return };
    if count > paste::MAX_DROPPED_FILES {
        dialogs::message(owner, "Send files", &format!("Drop at most {} files at once.", paste::MAX_DROPPED_FILES));
        return;
    }
    let nick = match kind {
        Kind::Query => name.clone(),
        Kind::Channel => match ui::user_at_screen(h, sx, sy) {
            Some(n) => n,
            None => {
                dialogs::info(owner, "Send files", "To send a file, drop it on a private chat, or on a person's name in the user list.");
                return;
            }
        },
        _ => {
            dialogs::info(owner, "Send files", "To send a file, drop it on a private chat, or on a person's name in a channel's user list.");
            return;
        }
    };
    if eq_ic(&nick, &nick_of(conn)) {
        dialogs::info(owner, "Send files", "That is you.");
        return;
    }
    for path in paths {
        let shown = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if path.is_dir() {
            dialogs::info(owner, "Send files", &format!("{shown} is a folder; only files can be sent."));
            continue;
        }
        let size = std::fs::metadata(&path).map(|m| human_size(m.len())).unwrap_or_else(|_| "unknown size".into());
        if dialogs::confirm(owner, "Send file", &format!("Send {shown} ({size}) to {nick} over DCC?")) {
            dcc::send_file(h, conn, &nick, path);
        }
    }
}

/// A word in chat text was double-clicked: a channel name goes to that channel (joining it when needed), and a
/// nickname of someone in this channel opens a chat with them.
pub fn chat_word_clicked(h: HWND, word: &str) {
    let Some((conn, kind)) = ui::child(h).map(|c| (c.conn, c.kind)) else { return };
    match crate::linkinfo::classify(word) {
        Some(crate::linkinfo::Named::Channel(chan)) => match window_for(conn, Kind::Channel, &chan, false) {
            Some(w) => ui::activate(hw(w)),
            None => {
                if with(|a| a.conns.get(&conn).is_some_and(|c| c.registered)) {
                    join_channel(conn, &chan);
                } else {
                    local(h, "* Not connected.");
                }
            }
        },
        Some(crate::linkinfo::Named::Nick(nick)) => {
            if kind == Kind::Channel && ui::has_member(h, &nick) && !eq_ic(&nick, &nick_of(conn)) {
                user_action(h, ui::UserAction::Query, nick);
            }
        }
        None => {}
    }
}

pub fn join_channel(conn: u32, name: &str) {
    if !send(conn, Message::new("JOIN", &[name])) {
        server_print(conn, "* Not connected.");
    }
}

pub fn join_dialog() {
    let owner = ui::frame();
    let Some(conn) = ui::active_child().and_then(|h| ui::child(h).map(|c| c.conn)) else { return };
    if !with(|a| a.conns.get(&conn).is_some_and(|c| c.registered)) {
        dialogs::message(owner, "Join channel", "Connect to a server first (File > Connect).");
        return;
    }
    let mut fields = vec![Field::text("Channel", "#"), Field::text("Key (optional)", "")];
    loop {
        let Some(v) = dialogs::show(owner, "Join channel", &fields) else { return };
        let chan = v[0].trim();
        if chan.len() < 2 || chan.contains(char::is_whitespace) || !is_channel(chan) {
            dialogs::message(owner, "Join channel", "Enter a channel name such as #chimerairc.");
            fields[0].value = v[0].clone();
            fields[1].value = v[1].clone();
            continue;
        }
        let key = v[1].trim();
        if key.is_empty() {
            send(conn, Message::new("JOIN", &[chan]));
        } else {
            send(conn, Message::new("JOIN", &[chan, key]));
        }
        return;
    }
}

pub fn prefs_dialog() {
    let owner = ui::frame();
    let s = settings();
    let fields = vec![
        Field::text("Default nickname", &s.nick),
        Field::text("Username", &s.user),
        Field::text("Real name", &s.realname),
        Field::text("Quit message", &s.quit_msg),
        Field::check("Show timestamps", s.timestamps),
        Field::check("Log conversations to files", s.logging),
        Field::text("Log folder", &log_base(&s.log_dir).to_string_lossy()),
        Field::text("DCC public IP", &s.dcc_ip),
        Field::text("DCC ports (e.g. 5000-5100)", &s.dcc_ports),
        Field::text("DCC reject types", &s.dcc_reject_exts),
        Field::text("DCC reject over MB", &s.dcc_max_mb.to_string()),
        Field::check("DCC auto-reject all chat requests", s.dcc_reject_chat),
        Field::text("DCC speed limit KB/s", &s.dcc_limit_kb.to_string()),
        Field::wide_check("Open the DCC port on my router (PCP, NAT-PMP, UPnP)", s.dcc_upnp),
        Field::text("History lines on join (0 = off)", &s.history_on_join.to_string()),
        Field::text("TLS client certificate (PEM file)", &s.client_cert),
        Field::text("Client key file (if separate)", &s.client_key),
    ];
    let Some(v) = dialogs::show(owner, "Preferences", &fields) else { return };
    let ok = |x: &str| !x.trim().is_empty() && !x.contains(char::is_whitespace);
    let mut bad_dcc: Vec<&str> = Vec::new();
    let mut bad_cert: Option<String> = None;
    with(|a| {
        let st = &mut a.settings;
        if ok(&v[0]) {
            st.nick = v[0].trim().to_owned();
        }
        if ok(&v[1]) {
            st.user = v[1].trim().to_owned();
        }
        if !v[2].trim().is_empty() {
            st.realname = v[2].trim().to_owned();
        }
        st.quit_msg = v[3].trim().to_owned();
        st.timestamps = v[4] == "1";
        st.logging = v[5] == "1";
        // Keep the setting empty while it is the default, so the default can move with the app.
        let dir = v[6].trim();
        st.log_dir = if dir.is_empty() || Path::new(dir) == default_log_dir() { String::new() } else { dir.to_owned() };
        // DCC: keep only values that parse, and say so if one was dropped.
        let (ip, ports) = (v[7].trim(), v[8].trim());
        if ip.is_empty() || ip.parse::<std::net::IpAddr>().is_ok() {
            st.dcc_ip = ip.to_owned();
        } else {
            bad_dcc.push("the public IP is not a valid address");
        }
        // Auto-reject rules: store the cleaned extension list; a bad size means "off".
        st.dcc_reject_exts = dccrules::parse_exts(&v[9]).join(",");
        match v[10].trim().parse::<u64>() {
            Ok(n) => st.dcc_max_mb = n,
            Err(_) if v[10].trim().is_empty() => st.dcc_max_mb = 0,
            Err(_) => bad_dcc.push("the size limit must be a whole number of MB"),
        }
        st.dcc_reject_chat = v[11] == "1";
        match v[12].trim().parse::<u64>() {
            Ok(n) => st.dcc_limit_kb = n,
            Err(_) if v[12].trim().is_empty() => st.dcc_limit_kb = 0,
            Err(_) => bad_dcc.push("the speed limit must be a whole number of KB/s (0 = unlimited)"),
        }
        st.dcc_upnp = v[13] == "1";
        match v[14].trim().parse::<u32>() {
            Ok(n) => st.history_on_join = n.min(500),
            Err(_) if v[14].trim().is_empty() => st.history_on_join = 0,
            Err(_) => bad_dcc.push("the history length must be a whole number of lines (0 = off)"),
        }
        // A client certificate is only stored if it can really be loaded (a typo would otherwise
        // show up as a mysterious login failure later).
        let (cert, key) = (v[15].trim(), v[16].trim());
        if cert.is_empty() {
            st.client_cert.clear();
            st.client_key.clear();
        } else {
            let key_path = (!key.is_empty()).then(|| PathBuf::from(key));
            match chimerairc_core::clientcert::load(Path::new(cert), key_path.as_deref()) {
                Ok(_) => {
                    st.client_cert = cert.to_owned();
                    st.client_key = key.to_owned();
                }
                Err(e) => bad_cert = Some(e),
            }
        }
        // takes effect at once, also for transfers that are already running
        chimerairc_core::dcc::set_rate_limit(st.dcc_limit_kb.saturating_mul(1024));
        if ports.is_empty() || parse_port_range(ports).is_some() {
            st.dcc_ports = ports.to_owned();
        } else {
            bad_dcc.push("the port range must look like 5000-5100 (ports 1024 and up)");
        }
        st.save();
    });
    sync_menu();
    if !bad_dcc.is_empty() {
        dialogs::message(owner, "Preferences", &format!("Not saved: {}.", bad_dcc.join("; ")));
    }
    if let Some(e) = bad_cert {
        dialogs::message(owner, "Preferences", &format!("The client certificate was not saved: {e}."));
    }
}

// ------------------------------------------------------------------ logging

fn default_log_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("ChimeraIRC").join("logs")
}

fn log_base(configured: &str) -> PathBuf {
    if configured.trim().is_empty() {
        default_log_dir()
    } else {
        PathBuf::from(configured.trim())
    }
}

/// Where this window's log lives (whether or not it exists yet).
fn log_path_of(h: HWND) -> Option<PathBuf> {
    let (kind, conn, name) = ui::child(h).map(|c| (c.kind, c.conn, c.name.clone()))?;
    let (host, base) = with(|a| (a.conns.get(&conn).map(|c| c.host.clone()), log_base(&a.settings.log_dir)));
    let host = host.filter(|h| !h.is_empty())?; // nothing to file it under before we connect
    let target = match kind {
        Kind::Server => logger::Target::Status,
        Kind::Channel => logger::Target::Channel(&name),
        // DCC chats are private conversations: filed with the queries as "=nick.log".
        Kind::Query | Kind::DccChat => logger::Target::Query(&name),
    };
    Some(logger::path_for(&base, &host, target))
}

/// Append a line printed in window `h` to its log, if logging is on.
fn log_line(h: isize, line: &str) {
    if !with(|a| a.settings.logging) {
        return;
    }
    let Some(path) = log_path_of(hw(h)) else { return };
    let plain = format::strip(line);
    let result = with(|a| a.logger.append(&path, &datetime_stamp(), &plain));
    if let Err(e) = result {
        // Say so once per run rather than on every line.
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !WARNED.swap(true, Ordering::Relaxed) {
            ui::print(hw(h), None, &format!("* Logging failed ({e}); check the log folder in Preferences."), LineKind::Error, &highlight::Marks::default());
        }
    }
}

pub fn set_logging(on: bool) {
    with(|a| {
        a.settings.logging = on;
        a.settings.save();
    });
    sync_menu();
}

/// Open a path with its associated program (Explorer for folders, the text editor for logs).
fn shell_open(path: &Path) {
    let p = wide(&path.to_string_lossy());
    unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            windows::core::w!("open"),
            windows::core::PCWSTR(p.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

pub fn open_log_folder() {
    let dir = log_base(&settings().log_dir);
    if std::fs::create_dir_all(&dir).is_ok() {
        shell_open(&dir);
    } else {
        dialogs::message(ui::frame(), "Logs", "The log folder could not be created. Check it in Preferences.");
    }
}

pub fn open_window_log() {
    let Some(h) = ui::active_child() else { return };
    match log_path_of(h) {
        Some(p) if p.exists() => shell_open(&p),
        _ => dialogs::message(ui::frame(), "Logs", "This window has no log yet. Turn on logging (Options > Toggle Logging) and it will be created as lines arrive."),
    }
}

pub fn highlights_dialog() {
    let st = settings();
    let current = hldialog::HighlightPrefs { nick: st.hl_nick, flash: st.hl_flash, rules: st.highlights };
    let Some(new) = hldialog::show(ui::frame(), &current) else { return };
    with(|a| {
        a.settings.hl_nick = new.nick;
        a.settings.hl_flash = new.flash;
        a.settings.highlights = new.rules;
        a.settings.save();
    });
}

/// Options > Sounds: which sound plays for which event.
pub fn sounds_dialog() {
    use crate::sounds::{self, EVENTS};
    let (chosen, background) = with(|a| (a.settings.sounds.clone(), a.settings.sounds_background));
    let mut values: Vec<String> = EVENTS.iter().map(|e| sounds::choice_for(&chosen, e.id).to_owned()).collect();
    let mut background = background;
    loop {
        let names: Vec<&str> = sounds::NAMES.iter().map(|(n, _)| *n).collect();
        let mut fields = vec![Field::note(&format!(
            "Highlight is a message that mentions your nick or matches a highlight rule. For each event write off, one of the Windows sounds ({}), or the path of a .wav file such as C:\\Sounds\\ping.wav. Use ... to choose a file and Play to hear it.",
            names.join(", ")
        ))];
        for (e, v) in EVENTS.iter().zip(&values) {
            fields.push(Field::sound(e.label, v));
        }
        fields.push(Field::wide_check("Only play sounds while ChimeraIRC is in the background", background));
        let Some(v) = dialogs::show(ui::frame(), "Sounds", &fields) else { return };
        values = (0..EVENTS.len()).map(|i| v[i + 1].trim().to_owned()).collect();
        background = v[EVENTS.len() + 1] == "1";
        let bad = EVENTS.iter().zip(&values).find_map(|(e, s)| sounds::check(s).err().map(|m| format!("{}: {m}", e.label)));
        if let Some(m) = bad {
            dialogs::message(ui::frame(), "Sounds", &m);
            continue;
        }
        with(|a| {
            a.settings.sounds.clear();
            for (e, s) in EVENTS.iter().zip(&values) {
                if s.as_str() != e.default {
                    a.settings.set_sound(e.id, if s.is_empty() { "off" } else { s });
                }
            }
            a.settings.sounds_background = background;
            a.settings.save();
        });
        return;
    }
}

/// `/sound` lists the events and their sounds, `/sound <name or file>` plays one, `/sound <event> <sound>` sets one.
fn sound_command(h: HWND, rest: &str) {
    use crate::sounds::{self, EVENTS};
    let rest = rest.trim();
    if rest.is_empty() {
        local(h, "* Sounds (change them under Options > Sounds, or /sound <event> <sound>; try one with /sound <name or file>):");
        for e in &EVENTS {
            local(h, &format!("  {:<12} {}   [{}]", e.id, settings().sound_for(e.id), e.label));
        }
        let names: Vec<&str> = sounds::NAMES.iter().map(|(n, _)| *n).collect();
        local(h, &format!("  Windows sounds: {}", names.join(", ")));
        return;
    }
    let (first, tail) = rest.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((rest, ""));
    if let Some(e) = sounds::event(&first.to_ascii_lowercase()) {
        if tail.is_empty() {
            local(h, &format!("* {}: {}", e.id, settings().sound_for(e.id)));
            return;
        }
        match sounds::check(tail) {
            Ok(_) => {
                with(|a| {
                    a.settings.set_sound(e.id, if tail.eq_ignore_ascii_case("none") { "off" } else { tail });
                    a.settings.save();
                });
                local(h, &format!("* {} sound: {tail}", e.id));
                sounds::play(tail);
            }
            Err(m) => local(h, &format!("* {m}")),
        }
        return;
    }
    match sounds::check(rest) {
        Ok(sounds::Spec::Off) => local(h, "* That is silent."),
        Ok(_) => sounds::play(rest),
        Err(m) => local(h, &format!("* {m}")),
    }
}

pub fn toggle_typing_show() {
    with(|a| {
        a.settings.typing_show = !a.settings.typing_show;
        a.settings.save();
    });
    sync_menu();
    refresh_status();
}

pub fn toggle_typing_send() {
    with(|a| {
        a.settings.typing_send = !a.settings.typing_send;
        a.settings.save();
    });
    sync_menu();
}

/// Tell `target` we are typing (or paused, or done), when the user allows it and the server supports it.
pub fn send_typing(conn: u32, target: &str, state: chimerairc_core::TypingState) {
    if !with(|a| a.settings.typing_send) {
        return;
    }
    if let Some(c) = with(|a| a.conns.get(&conn).and_then(|c| c.client.clone())) {
        c.typing(target, state);
    }
}

/// Once a second: our pauses in typing, and other people's typing that ran out.
pub fn typing_tick() {
    let ids: Vec<u32> = with(|a| a.conns.keys().copied().collect());
    let mut changed = false;
    for id in ids {
        for h in all_windows(id) {
            changed |= ui::typing_tick_window(hw(h));
        }
    }
    if changed {
        refresh_status();
    }
}

pub fn toggle_timestamps() {
    with(|a| {
        a.settings.timestamps = !a.settings.timestamps;
        a.settings.save();
    });
    sync_menu();
}

// ------------------------------------------------------------------ input

pub fn submit(h: HWND, text: String) {
    let Some((kind, conn, name)) = ui::child(h).map(|c| (c.kind, c.conn, c.name.clone())) else { return };
    if scripts::input(h, kind, conn, &name, &text) {
        return;
    }
    if let Some(cmd) = text.strip_prefix('/') {
        if !cmd.starts_with('/') {
            command(h, kind, conn, &name, cmd);
            return;
        }
    }
    say(h, kind, conn, &name, &text);
}

fn local(h: HWND, line: &str) {
    print(raw(h), line);
}

fn say(h: HWND, kind: Kind, conn: u32, target: &str, text: &str) {
    if kind == Kind::DccChat {
        dcc::chat_say(h, text);
        return;
    }
    if kind == Kind::Server {
        local(h, "* You are not in a channel. Use /join #channel or /query nick.");
        return;
    }
    send_chat(h, conn, target, "PRIVMSG", text);
}

/// Is `text`, sent to `target`, a login to a network service (`IDENTIFY <password>` to NickServ and the like)? Such
/// a message must never be shown, logged or kept: not by us and not as the server's echo of it.
fn is_secret_to_services(target: &str, text: &str) -> bool {
    !is_channel(target) && perform::looks_like_services(target) && perform::check_line(&format!("/msg {target} {text}")).is_err()
}

/// Send text as one or more PRIVMSG/NOTICE lines, echoing locally unless the server will.
fn send_chat(h: HWND, conn: u32, target: &str, cmd: &str, text: &str) {
    if is_secret_to_services(target, text) {
        send_secret_chat(h, conn, target, cmd, text);
        return;
    }
    let (echo, nick, up) = with(|a| {
        a.conns
            .get(&conn)
            .map(|c| (c.echo, c.nick.clone(), c.client.is_some()))
            .unwrap_or((false, String::new(), false))
    });
    if !up {
        local(h, "* Not connected.");
        return;
    }
    // what is said to a service goes to the server window, where the answers come, and reads as a command to it
    let service = !is_channel(target) && perform::looks_like_services(target);
    let mut dest = raw(h);
    if service && window_for(conn, Kind::Query, target, false).is_none() {
        if let Some(sw) = with(|a| a.conns.get(&conn).map(|c| c.server_win)).filter(|w| *w != 0) {
            dest = sw;
        }
    }
    for part in split_text(text, 400) {
        send(conn, Message::new(cmd, &[target, part]));
        if !echo {
            let line = if cmd == "NOTICE" {
                format!("-> -{target}- {part}")
            } else if service {
                format!("-> *{target}* {part}")
            } else {
                format!("<{nick}> {part}")
            };
            print_k(dest, LineKind::Normal, &line);
        }
    }
}

/// Send a login to a service that the user typed by hand: sent as it is, but only a masked line is shown (and
/// logged), and the server's own copy of it (`echo-message`) is dropped when it arrives.
fn send_secret_chat(h: HWND, conn: u32, target: &str, cmd: &str, text: &str) {
    if !with(|a| a.conns.get(&conn).is_some_and(|c| c.client.is_some())) {
        local(h, "* Not connected.");
        return;
    }
    expect_secret_echo(conn, target, text);
    send(conn, Message::new(cmd, &[target, text]));
    let shown = perform::mask_service_text(text);
    let line = if cmd == "NOTICE" { format!("-> -{target}- {shown}") } else { format!("-> *{target}* {shown}") };
    let ts = with(|a| a.settings.timestamps).then(timestamp);
    // shown without being logged or fed to a pane: the masked line would add nothing there
    ui::print(h, ts.as_deref(), &line, LineKind::Normal, &highlight::Marks::default());
}

fn command(h: HWND, kind: Kind, conn: u32, name: &str, cmd: &str) {
    let (verb, rest) = cmd.split_once(' ').unwrap_or((cmd, ""));
    let rest = rest.trim();
    let verb = verb.to_ascii_lowercase();
    let connected = with(|a| a.conns.get(&conn).is_some_and(|c| c.client.is_some()));
    let need = |h: HWND| -> bool {
        if !connected {
            local(h, "* Not connected. Use /server <host>.");
        }
        connected
    };
    match verb.as_str() {
        "help" | "?" => {
            for l in [
                "Commands:",
                "  /server [-plain] <host> [port] [nick]   connect (TLS by default; -plain = no TLS, default port 6667)",
                "  /join #chan[,#chan2] [key]    /part [#chan] [reason]    /query <nick> [text]",
                "  /msg <target> <text>    /notice <target> <text>    /me <action>",
                "  /list [words]  opens the Channel List (optionally pre-filtered)",
                "  /ignore [nick or mask]  hide someone (no argument lists)    /unignore <nick or mask>",
                "  /buddy [list]  /buddy add <nick> [group]  /buddy remove <nick>   watch when people come and go",
                "  /perform [list]  /perform edit [network]  /perform run   commands that run when a network connects",
                "  /history [lines]    /history before|after [point] [lines]    /history around <point>    /history between <point> <point>   chat history from the server (when it has it)",
                "  Pasting several lines asks first (and sends them about one a second; /stoppaste stops it). Drop a file on a private chat, or on a name in the user list, to send it by DCC.",
                "  /invite <nick> [#channel]    /reply <nick> <text>   answer that person's latest message    /react <nick> <emoji>   (/unreact takes it back)",
                "  /script   list the Lua scripts (/script reload, /script unload <name>, /script folder)   scripts can add commands of their own",
                "  /sound   the sounds for events (/sound <name or file> tries one, /sound <event> <sound> sets one; also Options > Sounds)",
                "  /alias name text   make your own command (/alias lists them, /alias -d name removes)    /uptime   how long the program and this connection have been up",
                "  /find <text>   find in this window (F3 next, Shift+F3 previous)    /logsearch <text>   search the saved logs    /identity   nickname, real name and SASL account per network",
                "  /monitor + <nick>    /monitor - <nick>    /monitor list   (the same as /buddy: you are told when they come and go)",
                "  /setname <real name>   change your real name on the server (when it supports it)",
                "  /nick <newnick>    /topic [text]    /quit [reason]    /raw <line>",
                "  /mode [target] <modes>    /away [reason] (alone: away, or back when away)    /back    /whois <nick>    /ns <command>    /cs <command>",
                "  /clear    /close",
            ] {
                local(h, l);
            }
        }
        "server" | "connect" => {
            let mut args: Vec<&str> = rest.split_whitespace().collect();
            let plain = args.first().is_some_and(|a| a.eq_ignore_ascii_case("-plain"));
            if plain {
                args.remove(0);
            }
            let Some(host) = args.first() else {
                local(h, "* Usage: /server [-plain] <host> [port] [nick]");
                return;
            };
            let port = match args.get(1) {
                Some(p) => match p.parse::<u16>() {
                    Ok(p) => p,
                    Err(_) => {
                        local(h, "* Invalid port.");
                        return;
                    }
                },
                None => {
                    if plain {
                        6667
                    } else {
                        6697
                    }
                }
            };
            let explicit = args.get(2).map(|s| s.to_string());
            let Some(cfg) = connect_config(host, port, !plain, explicit.as_deref()) else { return };
            connect(conn, cfg, Vec::new());
        }
        "ignore" => {
            if rest.is_empty() {
                ignore_list(h);
            } else {
                ignore_add(h, conn, rest);
            }
        }
        "unignore" => ignore_remove(h, rest),
        "perform" | "autoperform" => {
            let (sub, arg) = rest.split_once(char::is_whitespace).map(|(s, a)| (s, a.trim())).unwrap_or((rest, ""));
            match sub.to_ascii_lowercase().as_str() {
                "" | "list" => perform_list(h),
                "edit" => perform_dialog(if arg.is_empty() { None } else { Some(arg) }),
                "run" => {
                    let host = with(|a| a.conns.get(&conn).map(|c| c.host.clone())).unwrap_or_default();
                    let ready = with(|a| a.conns.get(&conn).is_some_and(|c| c.registered && c.client.is_some()));
                    match (ready, perform_script(&host)) {
                        (false, _) => local(h, "* Not connected yet."),
                        (true, None) => local(h, "* There is no auto-perform script for this network. Use /perform edit."),
                        (true, Some(lines)) => start_perform(conn, &host, lines, Vec::new()),
                    }
                }
                _ => local(h, "* Usage: /perform [list] | /perform edit [network] | /perform run"),
            }
        }
        "buddy" | "buddies" => {
            let (sub, arg) = rest.split_once(char::is_whitespace).map(|(s, a)| (s, a.trim())).unwrap_or((rest, ""));
            match sub.to_ascii_lowercase().as_str() {
                "" | "list" => buddy_list(h),
                "add" => {
                    let (n, g) = arg.split_once(char::is_whitespace).map(|(n, g)| (n, g.trim())).unwrap_or((arg, ""));
                    buddy_add(h, n, g)
                }
                "remove" | "del" | "delete" => buddy_remove(h, arg),
                _ => local(h, "* Usage: /buddy [list] | /buddy add <nick> [group] | /buddy remove <nick>"),
            }
        }
        "monitor" => {
            let (op, arg) = rest.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((rest, ""));
            match op {
                "+" | "add" if !arg.is_empty() => {
                    for nick in arg.split([',', ' ']).filter(|n| !n.is_empty()) {
                        buddy_add(h, nick, "");
                    }
                }
                "-" | "remove" | "del" if !arg.is_empty() => {
                    for nick in arg.split([',', ' ']).filter(|n| !n.is_empty()) {
                        buddy_remove(h, nick);
                    }
                }
                "" | "l" | "list" | "s" | "status" => buddy_list(h),
                _ => local(h, "* Usage: /monitor + <nick> | /monitor - <nick> | /monitor list"),
            }
        }
        "alias" => alias_command(h, rest),
        "sound" | "sounds" => sound_command(h, rest),
        "script" | "scripts" => scripts::command_script(h, rest),
        "invite" => v3::invite_command(h, kind, conn, name, rest),
        "reply" => v3::reply_command(h, rest),
        "react" => v3::react_command(h, rest, false),
        "unreact" => v3::react_command(h, rest, true),
        "stoppaste" => stop_paste(h),
        "say" => {
            if rest.is_empty() {
                local(h, "* Usage: /say <text>   (says the text in this window, even when it starts with a /)");
            } else {
                say(h, kind, conn, name, rest);
            }
        }
        "uptime" => {
            let conn_part = with(|a| a.conns.get(&conn).and_then(|c| c.registered_at.map(|t| (c.host.clone(), t.elapsed().as_secs()))));
            let mut line = format!("* ChimeraIRC has been running for {}", alias::duration_text(crate::util::uptime_secs()));
            if let Some((host, secs)) = conn_part {
                line.push_str(&format!("; connected to {host} for {}", alias::duration_text(secs)));
            }
            local(h, &format!("{line}."));
        }
        "find" => {
            if rest.is_empty() {
                find_dialog();
            } else {
                LAST_FIND.with(|l| *l.borrow_mut() = (rest.to_owned(), false));
                find_again(true);
            }
        }
        "logsearch" => {
            if rest.is_empty() {
                log_search_dialog();
            } else {
                run_log_search(rest, false, false);
            }
        }
        "identity" => identity_dialog(None),
        "setname" => {
            if !need(h) {
                return;
            }
            if rest.is_empty() {
                local(h, "* Usage: /setname <new real name>");
            } else if !with(|a| a.conns.get(&conn).is_some_and(|c| c.setname)) {
                local(h, "* This server does not let you change your real name while connected.");
            } else {
                send(conn, Message::new("SETNAME", &[rest]));
            }
        }
        "list" => {
            if need(h) {
                open_channel_list(conn, if rest.is_empty() { None } else { Some(rest) });
            }
        }
        "join" | "j" => {
            if !need(h) {
                return;
            }
            if rest.is_empty() {
                local(h, "* Usage: /join #channel [key]");
                return;
            }
            let args: Vec<&str> = rest.split_whitespace().collect();
            send(conn, Message::new("JOIN", &args));
        }
        "part" | "leave" => {
            if !need(h) {
                return;
            }
            let (chan, reason) = if rest.starts_with(['#', '&', '+', '!']) {
                let (c, r) = rest.split_once(' ').unwrap_or((rest, ""));
                (c.to_owned(), r.trim().to_owned())
            } else if kind == Kind::Channel {
                (name.to_owned(), rest.to_owned())
            } else {
                local(h, "* Usage: /part [#channel] [reason]");
                return;
            };
            if reason.is_empty() {
                send(conn, Message::new("PART", &[&chan]));
            } else {
                send(conn, Message::new("PART", &[&chan, &reason]));
            }
        }
        "msg" | "privmsg" | "notice" => {
            if !need(h) {
                return;
            }
            let Some((target, text)) = rest.split_once(' ') else {
                local(h, &format!("* Usage: /{verb} <target> <text>"));
                return;
            };
            let text = text.trim();
            let wh = if is_channel(target) {
                window_for(conn, Kind::Channel, target, false)
            } else if verb == "notice" {
                None
            } else if perform::looks_like_services(target) {
                // a message to a service (NickServ, ChanServ, ...) does not open a chat window of its own: it is sent
                // from this window and the answers come to the server window; a chat the user opened keeps working
                window_for(conn, Kind::Query, target, false)
            } else {
                window_for(conn, Kind::Query, target, true)
            };
            let cmdw = if verb == "notice" { "NOTICE" } else { "PRIVMSG" };
            send_chat(wh.map(hw).unwrap_or(h), conn, target, cmdw, text);
        }
        "query" | "q" => {
            let (nick, text) = rest.split_once(' ').unwrap_or((rest, ""));
            if nick.is_empty() || is_channel(nick) {
                local(h, "* Usage: /query <nick> [text]");
                return;
            }
            if let Some(qh) = window_for(conn, Kind::Query, nick, true) {
                if !text.trim().is_empty() && need(h) {
                    send_chat(hw(qh), conn, nick, "PRIVMSG", text.trim());
                }
            }
        }
        "dcc" => dcc::command(h, conn, rest),
        "me" | "action" => {
            if kind == Kind::DccChat {
                dcc::chat_action(h, rest);
                return;
            }
            if kind == Kind::Server {
                local(h, "* /me only works in a channel or query window.");
                return;
            }
            if !need(h) {
                return;
            }
            let body = format!("\x01ACTION {rest}\x01");
            send(conn, Message::new("PRIVMSG", &[name, &body]));
            let (echo, nick) = with(|a| a.conns.get(&conn).map(|c| (c.echo, c.nick.clone())).unwrap_or_default());
            if !echo {
                print_k(raw(h), LineKind::Action, &format!("* {nick} {rest}"));
            }
        }
        "mode" => {
            if !need(h) {
                return;
            }
            let parts: Vec<&str> = rest.split_whitespace().collect();
            let me = nick_of(conn);
            // `/mode +i` in a channel window is about the channel, in any other window about yourself;
            // `/mode #chan +m` and `/mode Alice +i` name the target
            let names_target = parts.first().is_some_and(|p| !p.starts_with(['+', '-']));
            let (target, modes): (String, &[&str]) = if names_target {
                (parts[0].to_owned(), &parts[1..])
            } else if kind == Kind::Channel {
                (name.to_owned(), &parts[..])
            } else {
                (me, &parts[..])
            };
            let mut params: Vec<&str> = vec![&target];
            params.extend_from_slice(modes);
            send(conn, Message::new("MODE", &params));
        }
        "away" => {
            if !need(h) {
                return;
            }
            if !rest.is_empty() {
                send(conn, Message::new("AWAY", &[rest]));
            } else if with(|a| a.conns.get(&conn).is_some_and(|c| c.away)) {
                // /away on its own while away: come back (the same as /back)
                send(conn, Message::new("AWAY", &[]));
            } else {
                // an AWAY with no text would mean "not away" to the server, so there is always some text
                send(conn, Message::new("AWAY", &["Away"]));
            }
        }
        "back" => {
            if need(h) {
                send(conn, Message::new("AWAY", &[]));
            }
        }
        "whois" | "wi" => {
            if !need(h) {
                return;
            }
            if rest.is_empty() {
                local(h, "* Usage: /whois <nick>");
            } else {
                v3::send_from(h, conn, Message::new("WHOIS", &[rest]));
            }
        }
        "ns" | "nickserv" | "cs" | "chanserv" => {
            if rest.is_empty() {
                local(h, &format!("* Usage: /{verb} <command>   (a private message to the {} service)", if verb.starts_with('n') { "NickServ" } else { "ChanServ" }));
                return;
            }
            let service = if verb.starts_with('n') { "NickServ" } else { "ChanServ" };
            command(h, kind, conn, name, &format!("msg {service} {rest}"));
        }
        "nick" => {
            if rest.is_empty() || rest.contains(' ') {
                local(h, "* Usage: /nick <newnick>");
            } else if connected {
                send(conn, Message::new("NICK", &[rest]));
            } else {
                // Not connected yet: remember it for the next /server.
                with(|a| {
                    if let Some(c) = a.conns.get_mut(&conn) {
                        c.nick = rest.to_owned();
                    }
                });
                local(h, &format!("* Nick set to {rest} for the next connection."));
                refresh_status();
            }
        }
        "history" => {
            if !need(h) {
                return;
            }
            if !matches!(kind, Kind::Channel | Kind::Query) {
                local(h, "* /history works in a channel or chat window.");
            } else if !with(|a| a.conns.get(&conn).is_some_and(|c| c.history)) {
                local(h, "* This server does not offer chat history.");
            } else {
                match history::parse_command(rest) {
                    Err(e) => local(h, &format!("* {e}")),
                    Ok(history::Request::Latest(n)) => {
                        send(conn, Message::new("CHATHISTORY", &["LATEST", name, "*", &n.to_string()]));
                    }
                    Ok(history::Request::Before(point, n)) => {
                        let point = point.or_else(|| {
                            with(|a| a.conns.get(&conn).and_then(|c| c.history_oldest.get(&name.to_lowercase()).map(|t| format!("timestamp={t}"))))
                        });
                        match point {
                            Some(p) => {
                                send(conn, Message::new("CHATHISTORY", &["BEFORE", name, &p, &n.to_string()]));
                            }
                            None => local(h, "* No history is loaded here yet; use /history first, or give a point: /history before <time|msgid=ID>."),
                        }
                    }
                    Ok(history::Request::After(point, n)) => {
                        let point = point.or_else(|| {
                            with(|a| a.conns.get(&conn).and_then(|c| c.history_newest.get(&name.to_lowercase()).map(|t| format!("timestamp={t}"))))
                        });
                        match point {
                            Some(p) => {
                                send(conn, Message::new("CHATHISTORY", &["AFTER", name, &p, &n.to_string()]));
                            }
                            None => local(h, "* No history is loaded here yet; use /history first, or give a point: /history after <time|msgid=ID>."),
                        }
                    }
                    Ok(history::Request::Around(p, n)) => {
                        send(conn, Message::new("CHATHISTORY", &["AROUND", name, &p, &n.to_string()]));
                    }
                    Ok(history::Request::Between(a, b, n)) => {
                        send(conn, Message::new("CHATHISTORY", &["BETWEEN", name, &a, &b, &n.to_string()]));
                    }
                }
            }
        }
        "topic" => {
            if kind != Kind::Channel {
                local(h, "* /topic only works in a channel window.");
            } else if need(h) {
                if rest.is_empty() {
                    send(conn, Message::new("TOPIC", &[name]));
                } else {
                    send(conn, Message::new("TOPIC", &[name, rest]));
                }
            }
        }
        "quit" | "disconnect" => {
            if rest.is_empty() {
                disconnect(conn);
            } else if let Some(c) = with(|a| a.conns.get(&conn).and_then(|c| c.client.clone())) {
                stop_reconnecting(conn);
                c.quit(Some(rest));
            } else {
                local(h, "* Not connected.");
            }
        }
        "raw" | "quote" => {
            if !need(h) {
                return;
            }
            match Message::parse(rest) {
                Ok(m) => {
                    // a login to a service sent by hand: its echo from the server must not show the password
                    if matches!(m.command.as_str(), "PRIVMSG" | "NOTICE") {
                        if let [target, text] = m.params.as_slice() {
                            if is_secret_to_services(target, text) {
                                expect_secret_echo(conn, target, text);
                            }
                        }
                    }
                    if !send(conn, m) {
                        local(h, "* Not connected.");
                    }
                }
                Err(e) => local(h, &format!("* Bad raw line: {e}")),
            }
        }
        "clear" => unsafe {
            if let Some(c) = ui::child(h) {
                let _ = SetWindowTextW(c.output, windows::core::w!(""));
            }
        },
        "close" => unsafe {
            let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        _ => {
            if !run_alias(h, &verb, rest) {
                if !scripts::command(h, kind, conn, name, &verb, rest) {
                    local(h, &format!("* Unknown command /{verb}. Try /help."));
                }
            }
        }
    }
}

// ------------------------------------------------------------------ events

/// Ask the UI thread to run `drain_events` (once, however many events are queued).
fn wake_ui() {
    if !POSTED.swap(true, Ordering::AcqRel) {
        unsafe {
            let _ = PostMessageW(ui::frame(), ui::WM_APP_EVENT, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn drain_events() {
    POSTED.store(false, Ordering::Release);
    loop {
        let next = QUEUE.lock().unwrap().pop_front();
        let Some((conn, ev)) = next else { break };
        on_event(conn, ev);
    }
    loop {
        let next = RETRIES.lock().unwrap().pop_front();
        let Some((conn, generation)) = next else { break };
        retry_now(conn, generation);
    }
    loop {
        let next = PERFORM.lock().unwrap().pop_front();
        let Some((conn, generation, steps)) = next else { break };
        run_perform(conn, generation, steps);
    }
    dcc::drain_events();
    refresh_monitors();
    refresh_status();
}

// Transfers window plumbing (the window lives in dccwin.rs; the data in dcc.rs).
fn dccwin_open() {
    crate::dccwin::open(ui::frame());
}

fn dccwin_refresh() {
    crate::dccwin::refresh();
}

pub fn open_transfers_window() {
    dccwin_open();
}

pub use dcc::{
    cancel as dcc_cancel, clear_finished as dcc_clear_finished, open_file as dcc_open_file, open_log as dcc_open_log,
    rows as dcc_rows, show_in_folder as dcc_show,
};

use crate::capsfmt::{join_details, split_line};
use crate::alias;
use crate::identity::{self, Identity};
use crate::logsearch;
use crate::paste;

fn fmt_modes(changes: &[chimerairc_core::ModeChange]) -> String {
    let mut modes = String::new();
    let mut args = Vec::new();
    let mut cur = None;
    for c in changes {
        if cur != Some(c.adding) {
            modes.push(if c.adding { '+' } else { '-' });
            cur = Some(c.adding);
        }
        modes.push(c.mode);
        if let Some(a) = &c.arg {
            args.push(a.as_str());
        }
    }
    if args.is_empty() {
        modes
    } else {
        format!("{modes} {}", args.join(" "))
    }
}

/// Print a batch of history in the window for `target`, oldest line first, in the grey status
/// style and with the server's own times. Lines already shown are skipped, and history is not
/// logged (it was logged when it first happened).
fn show_history(conn: u32, target: &str, lines: Vec<chimerairc_core::HistoryLine>) {
    let h = if is_channel(target) {
        window_for(conn, Kind::Channel, target, false)
    } else {
        window_for_ex(conn, Kind::Query, target, true, false)
    };
    let Some(h) = h else { return };
    let fresh: Vec<chimerairc_core::HistoryLine> = with(|a| match a.conns.get_mut(&conn) {
        Some(c) => lines.into_iter().filter(|l| l.msgid.as_deref().map_or(true, |id| c.seen.mark(id))).collect(),
        None => Vec::new(),
    });
    if fresh.is_empty() {
        print(h, &format!("* No new history for {target}."));
        return;
    }
    if let Some(t) = fresh.first().and_then(|l| l.time.clone()) {
        with(|a| {
            if let Some(c) = a.conns.get_mut(&conn) {
                let key = target.to_lowercase();
                if c.history_oldest.get(&key).map_or(true, |old| history::earlier(&t, old)) {
                    c.history_oldest.insert(key, t);
                }
            }
        });
    }
    if let Some(t) = fresh.iter().filter_map(|l| l.time.clone()).max() {
        with(|a| {
            if let Some(c) = a.conns.get_mut(&conn) {
                let key = target.to_lowercase();
                if c.history_newest.get(&key).map_or(true, |new| history::earlier(new, &t)) {
                    c.history_newest.insert(key, t);
                }
            }
        });
    }
    let stamps = with(|a| a.settings.timestamps);
    print(h, &format!("* --- {} line{} of history for {target} ---", fresh.len(), if fresh.len() == 1 { "" } else { "s" }));
    for l in &fresh {
        let who = &l.source.name;
        let line = match l.kind {
            ChatKind::Message => format!("<{who}> {}", l.text),
            ChatKind::Action => format!("* {who} {}", l.text),
            ChatKind::Notice => format!("-{who}- {}", l.text),
        };
        let stamp = if stamps { l.time.as_deref().and_then(crate::util::history_stamp) } else { None };
        ui::print(hw(h), stamp.as_deref(), &line, LineKind::Info, &highlight::Marks::default());
    }
    print(h, "* --- end of history ---");
}

/// The channels this connection has windows open for.
fn open_channels(conn: u32) -> Vec<String> {
    let mut out: Vec<String> = all_windows(conn)
        .into_iter()
        .filter_map(|h| ui::child(hw(h)).filter(|c| c.kind == Kind::Channel).map(|c| c.name.clone()))
        .collect();
    out.sort_by_key(|c| c.to_lowercase());
    out.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    out
}

/// After a connection was lost: unless the user asked for it, or this connection never worked,
/// try again after a growing delay.
fn schedule_reconnect(conn: u32) {
    enum Plan {
        No,
        GiveUp(u32),
        Wait(std::time::Duration, u32, u32, u32),
    }
    let plan = with(|a| {
        let Some(c) = a.conns.get_mut(&conn) else { return Plan::No };
        if c.want_quit || c.last_cfg.is_none() || !(c.ever_registered || c.retrying) {
            return Plan::No;
        }
        c.retrying = true;
        match c.backoff.next_delay(reconnect::random_unit()) {
            Some(d) => Plan::Wait(d, c.backoff.attempts(), c.backoff.max_attempts(), c.retry_gen),
            None => {
                c.retrying = false;
                Plan::GiveUp(c.backoff.max_attempts())
            }
        }
    });
    match plan {
        Plan::No => {}
        Plan::GiveUp(n) => {
            for h in all_windows(conn) {
                print(h, &format!("* Giving up after {n} attempts to reconnect. Use /server to connect again."));
            }
        }
        Plan::Wait(delay, attempt, max, generation) => {
            let secs = delay.as_secs().max(1);
            for h in all_windows(conn) {
                print(h, &format!("* Reconnecting in {secs} s (attempt {attempt} of {max}). /quit stops it."));
            }
            with(|a| {
                a.rt.spawn(async move {
                    tokio::time::sleep(delay).await;
                    RETRIES.lock().unwrap().push_back((conn, generation));
                    wake_ui();
                })
            });
        }
    }
}

/// A reconnect timer ran out: connect again with the same settings and rejoin the channels.
fn retry_now(conn: u32, generation: u32) {
    let job = with(|a| {
        a.conns.get(&conn).and_then(|c| {
            let valid = c.retry_gen == generation && c.client.is_none() && !c.want_quit && c.retrying;
            valid.then(|| c.last_cfg.clone().map(|cfg| (cfg, c.rejoin.clone()))).flatten()
        })
    });
    if let Some((cfg, rejoin)) = job {
        connect_now(conn, cfg, rejoin);
    }
}

fn on_event(conn: u32, ev: Event) {
    let Some(me) = with(|a| a.conns.get(&conn).map(|c| c.nick.clone())) else { return };
    match ev {
        Event::Connected => server_print(conn, "* Connected. Registering..."),
        Event::Registered { nick } => {
            play_sound("connected", "");
            scripts::event(conn, None, "", "connect", &[("nick", nick.as_str().into())]);
            with(|a| {
                if let Some(c) = a.conns.get_mut(&conn) {
                    c.nick = nick.clone();
                    c.registered = true;
                    c.registered_at = Some(std::time::Instant::now());
                    c.away = false; // a new registration starts not away
                    c.ever_registered = true;
                    c.retrying = false;
                    c.backoff.reset();
                }
            });
            server_print(conn, &format!("* Registered as {nick}"));
            let chans = with(|a| a.conns.get_mut(&conn).map(|c| std::mem::take(&mut c.autojoin)).unwrap_or_default());
            let host = with(|a| a.conns.get(&conn).map(|c| c.host.clone())).unwrap_or_default();
            match perform_script(&host) {
                // a script runs first and then the favorite's channels are joined, so that identifying can come first
                Some(lines) => start_perform(conn, &host, lines, chans),
                None => {
                    for group in chans.chunks(8) {
                        send(conn, Message::new("JOIN", &[&group.join(",")]));
                    }
                }
            }
        }
        Event::CapsEnabled { enabled } => {
            with(|a| {
                if let Some(c) = a.conns.get_mut(&conn) {
                    c.echo = enabled.iter().any(|e| e == "echo-message");
                    c.history = enabled.iter().any(|e| e == "chathistory" || e == "draft/chathistory");
                    c.setname = enabled.iter().any(|e| e == "setname");
                }
            });
            v3::caps(conn, &enabled);
            if !enabled.is_empty() {
                server_print(conn, &format!("* Capabilities: {}", enabled.join(", ")));
            }
        }
        Event::SaslResult { ok, text } => {
            server_print(conn, &format!("* SASL {}: {text}", if ok { "succeeded" } else { "failed" }));
        }
        Event::Label { label } => v3::label(conn, label),
        Event::Invite { by, nick, channel } => v3::on_invite(conn, &me, &by, &nick, &channel),
        Event::React { nick, target, msgid, emoji, remove } => v3::on_react(conn, &me, &nick, &target, &msgid, &emoji, remove),
        Event::Chat { source, target, text, kind, msgid, time, reply_to } => {
            let stamp = time.as_deref().and_then(crate::util::server_time_stamp);
            if eq_ic(&source.name, &me) && is_secret_echo(conn, &target, &text) {
                return;
            }
            if let Some(id) = msgid.as_deref() {
                with(|a| a.conns.get_mut(&conn).map(|c| c.seen.mark(id)));
            }
            let from_me = eq_ic(&source.name, &me);
            // someone who speaks is no longer typing
            if !from_me && kind != ChatKind::Notice {
                let tw = if is_channel(&target) {
                    window_for(conn, Kind::Channel, &target, false)
                } else {
                    window_for(conn, Kind::Query, &source.name, false)
                };
                if let Some(h) = tw {
                    if ui::typing_spoke(hw(h), &source.name) && ui::active_child() == Some(hw(h)) {
                        refresh_status();
                    }
                }
            }
            // remember it (so it can be replied to) and, for a reply, say what it answers
            let reply_ctx = if kind == ChatKind::Notice {
                None
            } else {
                let peer = if from_me { target.as_str() } else { source.name.as_str() };
                v3::note_chat(conn, &v3::conversation_key(&target, peer), &source.name, &text, msgid.as_deref(), reply_to.as_deref())
            };
            let line = match (kind, &reply_ctx) {
                (ChatKind::Message, Some(c)) => format!("<{}> {} {}", source.name, c, text),
                (ChatKind::Action, Some(c)) => format!("* {} {} {}", source.name, c, text),
                (ChatKind::Message, None) => format!("<{}> {}", source.name, text),
                (ChatKind::Action, None) => format!("* {} {}", source.name, text),
                (ChatKind::Notice, _) => format!("-{}- {}", source.name, text),
            };
            // what we said to a service reads as a command to it, not as chat: `-> *NickServ* help`
            let to_service = from_me && !is_channel(&target) && perform::looks_like_services(&target);
            let line = if to_service && kind == ChatKind::Message { format!("-> *{target}* {text}") } else { line };
            // the Notices and Messages panes
            if kind == ChatKind::Notice {
                feed_notice(&source.name, &target, &text);
            } else if !is_channel(&target) && !from_me {
                feed_message(conn, &source.name, &text, kind == ChatKind::Action);
            }
            let lk = match kind {
                ChatKind::Action => LineKind::Action,
                ChatKind::Notice => LineKind::Notice,
                ChatKind::Message => LineKind::Normal,
            };
            // Highlights apply to what other people say (not notices, not our own echo); the
            // offsets are computed on the message body and shifted past the `<nick> ` prefix.
            let marks = if from_me || kind == ChatKind::Notice {
                highlight::Marks::default()
            } else {
                let prefix_len = line.len() - text.len();
                let here = if is_channel(&target) { highlight::Place::Channel(&target) } else { highlight::Place::Private };
                highlight::scan(&format::strip(&text), &highlight_rules(&me), here).shifted(prefix_len)
            };
            if !marks.is_empty() {
                notify_highlight(&marks.sound);
            } else if !from_me && kind != ChatKind::Notice && !is_channel(&target) && !perform::looks_like_services(&source.name) {
                play_sound("private", "");
            }
            if is_channel(&target) {
                match window_for(conn, Kind::Channel, &target, false) {
                    Some(h) => {
                        print_marked_at(h, lk, &line, &marks, stamp.as_deref());
                        if kind != ChatKind::Notice && !from_me {
                            ui::note_speaker(hw(h), &source.name);
                        }
                    }
                    None => server_print(conn, &format!("[{target}] {line}")),
                }
            } else if kind == ChatKind::Notice {
                if let Some(h) = with(|a| a.conns.get(&conn).map(|c| c.server_win)).filter(|h| *h != 0) {
                    print_k(h, LineKind::Notice, &line);
                }
            } else {
                let peer = if from_me { &target } else { &source.name };
                // Services (NickServ and friends) answer in the server window instead of opening a chat window
                // with each of them; a chat the user opened themselves keeps getting its messages.
                let has_window = window_for(conn, Kind::Query, peer, false).is_some();
                if !has_window && perform::looks_like_services(peer) {
                    if let Some(h) = with(|a| a.conns.get(&conn).map(|c| c.server_win)).filter(|h| *h != 0) {
                        print_marked_at(h, lk, &line, &marks, stamp.as_deref());
                    }
                // An incoming message must not steal focus; its tab lights up instead. Our own
                // echoed lines come from something the user just did, so those may take focus.
                } else if let Some(h) = window_for_ex(conn, Kind::Query, peer, true, from_me) {
                    print_marked_at(h, lk, &line, &marks, stamp.as_deref());
                }
            }
            scripts::message(conn, &source.name, &target, &text, kind, from_me, msgid.as_deref(), reply_to.as_deref());
        }
        Event::StsPolicy { duration, port, .. } => on_sts_policy(conn, duration, port),
        Event::StsUpgrade { port } => {
            with(|a| {
                if let Some(c) = a.conns.get_mut(&conn) {
                    c.sts_upgrade = Some(port);
                }
            });
            server_print(conn, &format!("* STS: the server requires TLS; reconnecting securely on port {port}..."));
        }
        Event::History { target, lines } => show_history(conn, &target, lines),
        Event::Ctcp { source, ctcp, reply, .. } => {
            // With echo-message the server sends our own CTCP back to us. It is not an incoming
            // request (and our own DCC offer must never prompt us to accept it).
            if eq_ic(&source.name, &me) {
                return;
            }
            if !reply && dcc::on_ctcp(conn, &source, &ctcp) {
                return; // a DCC offer: handled (and confirmed with the user) there
            }
            let what = if reply { "reply" } else { "request" };
            server_print(conn, &format!("* CTCP {what} from {}: {} {}", source.name, ctcp.command, ctcp.args));
        }
        Event::Join { channel, nick, account, realname } => {
            if eq_ic(&nick, &me) {
                if let Some(h) = window_for(conn, Kind::Channel, &channel, true) {
                    ui::clear_members(hw(h));
                    if with(|a| a.conns.get(&conn).is_some_and(|c| c.away)) {
                        ui::set_away(hw(h), &me, true); // a channel joined while away lists us as away too
                    }
                    print(h, &format!("* Now talking on {channel}"));
                }
                scripts::event(conn, window_for(conn, Kind::Channel, &channel, false), &channel, "join", &[("nick", me.as_str().into()), ("channel", channel.as_str().into()), ("mine", true.into())]);
            } else if let Some(h) = window_for(conn, Kind::Channel, &channel, false) {
                // extended-join: who they are logged in as, and their real name
                print(h, &format!("* {nick}{} has joined {channel}", join_details(account.as_deref(), realname.as_deref())));
                scripts::event(conn, Some(h), &channel, "join", &[("nick", nick.as_str().into()), ("channel", channel.as_str().into()), ("mine", false.into())]);
            }
        }
        Event::Part { channel, nick, reason } => {
            if let Some(h) = window_for(conn, Kind::Channel, &channel, false) {
                let r = reason.map(|r| format!(" ({r})")).unwrap_or_default();
                print(h, &format!("* {nick} has left {channel}{r}"));
                scripts::event(conn, Some(h), &channel, "part", &[("nick", nick.as_str().into()), ("channel", channel.as_str().into()), ("mine", eq_ic(&nick, &me).into())]);
            }
        }
        Event::Quit { nick, reason, channels } => {
            let r = reason.map(|r| format!(" ({r})")).unwrap_or_default();
            for c in channels {
                if let Some(h) = window_for(conn, Kind::Channel, &c, false) {
                    print(h, &format!("* {nick} has quit{r}"));
                }
            }
        }
        Event::Kick { channel, by, victim, reason } => {
            if let Some(h) = window_for(conn, Kind::Channel, &channel, false) {
                let r = reason.map(|r| format!(" ({r})")).unwrap_or_default();
                print(h, &format!("* {victim} was kicked by {by}{r}"));
            }
        }
        Event::Nick { old, new } => {
            if eq_ic(&old, &me) {
                with(|a| {
                    if let Some(c) = a.conns.get_mut(&conn) {
                        c.nick = new.clone();
                    }
                });
            }
            for h in all_windows(conn) {
                if ui::has_member(hw(h), &old) || (eq_ic(&old, &me) && ui::child(hw(h)).is_some_and(|c| c.kind == Kind::Server)) {
                    print(h, &format!("* {old} is now known as {new}"));
                }
            }
        }
        Event::Topic { channel, topic } => {
            if let Some(h) = window_for(conn, Kind::Channel, &channel, false) {
                ui::set_topic(hw(h), topic.as_deref().unwrap_or(""));
                match topic {
                    Some(t) => print(h, &format!("* Topic: {t}")),
                    None => print(h, "* Topic cleared"),
                }
            }
        }
        Event::NamesDone { .. } => {}
        Event::Members { channel, members } => {
            if let Some(h) = window_for(conn, Kind::Channel, &channel, false) {
                ui::set_members(hw(h), &members);
            }
        }
        Event::Mode { target, by, changes } => {
            let line = format!("* {} sets mode {}", by.unwrap_or_else(|| "server".into()), fmt_modes(&changes));
            match window_for(conn, Kind::Channel, &target, false) {
                Some(h) => print(h, &line),
                None => server_print(conn, &format!("[{target}] {line}")),
            }
        }
        Event::Presence { nick, online, mask, announce } => {
            let host = with(|a| {
                a.conns.get_mut(&conn).map(|c| {
                    c.presence.insert(nick.to_ascii_lowercase(), (online, mask.clone()));
                    c.host.clone()
                })
            })
            .unwrap_or_default();
            if announce {
                let line = format!("* Buddy {nick} is now {}{}", if online { "online" } else { "offline" }, if host.is_empty() { String::new() } else { format!(" ({host})") });
                server_print(conn, &line);
                dock::event(&line);
            }
            refresh_buddies();
        }
        // away-notify: the nick list marks who is away (no line in the chat: it would be noise)
        Event::SetName { nick, realname } => {
            if eq_ic(&nick, &me) {
                server_print(conn, &format!("* Your real name is now {realname}"));
            } else {
                for h in all_windows(conn) {
                    if ui::is_channel_window(hw(h)) && ui::has_member(hw(h), &nick) {
                        print(h, &format!("* {nick} changed their real name to {realname}"));
                    }
                }
            }
        }
        Event::Typing { nick, target, state } => {
            if !with(|a| a.settings.typing_show) {
                return;
            }
            let win = if is_channel(&target) {
                window_for(conn, Kind::Channel, &target, false)
            } else if eq_ic(&target, &me) {
                window_for(conn, Kind::Query, &nick, false)
            } else {
                None
            };
            if let Some(h) = win {
                if ui::typing_in(hw(h), &nick, state) && ui::active_child() == Some(hw(h)) {
                    refresh_status();
                }
            }
        }
        Event::Away { nick, message } => {
            let away = message.is_some();
            for h in all_windows(conn) {
                ui::set_away(hw(h), &nick, away);
            }
        }
        // account-notify and chghost: a line in each channel the person is in
        Event::Account { nick, account } => {
            let line = match &account {
                Some(a) => format!("* {nick} is now logged in as {a}"),
                None => format!("* {nick} has logged out"),
            };
            for h in all_windows(conn) {
                if ui::is_channel_window(hw(h)) && ui::has_member(hw(h), &nick) {
                    print(h, &line);
                }
            }
        }
        Event::ChgHost { nick, user, host } => {
            for h in all_windows(conn) {
                if ui::is_channel_window(hw(h)) && ui::has_member(hw(h), &nick) {
                    print(h, &format!("* {nick} is now {user}@{host}"));
                }
            }
        }
        // batch (netsplit, netjoin): one line for the whole group instead of one per person
        Event::NetSplit { servers, nicks, channels } => {
            let line = split_line(true, &servers, &nicks);
            for c in channels {
                if let Some(h) = window_for(conn, Kind::Channel, &c, false) {
                    print(h, &line);
                }
            }
        }
        Event::NetJoin { servers, nicks, channels } => {
            let line = split_line(false, &servers, &nicks);
            for c in channels {
                if let Some(h) = window_for(conn, Kind::Channel, &c, false) {
                    print(h, &line);
                }
            }
        }
        // A LIST reply may arrive without us asking (e.g. via /raw): open the window for it.
        Event::ListStart => {
            let h = match chanlist_window(conn) {
                Some(h) => Some(h),
                None => {
                    let host = with(|a| a.conns.get(&conn).map(|c| c.host.clone())).unwrap_or_default();
                    let h = chanwin::create(ui::frame(), conn, &host);
                    (!h.0.is_null()).then(|| {
                        with(|a| {
                            if let Some(c) = a.conns.get_mut(&conn) {
                                c.chanlist = raw(h);
                            }
                        });
                        h
                    })
                }
            };
            if let Some(h) = h {
                chanwin::begin(h);
            }
        }
        Event::ListEntry { channel, users, topic } => {
            if let Some(h) = chanlist_window(conn) {
                chanwin::push(h, chanlist::Entry::new(&channel, users, &topic));
            }
        }
        Event::ListEnd => {
            if let Some(h) = chanlist_window(conn) {
                chanwin::finish(h);
            }
        }
        Event::Numeric { code, params } => {
            // 306: now marked away; 305: no longer. Our own name in the nick lists follows (the server does not send
            // away-notify about ourselves).
            if code == 305 || code == 306 {
                let away = code == 306;
                with(|a| {
                    if let Some(c) = a.conns.get_mut(&conn) {
                        c.away = away;
                    }
                });
                for h in all_windows(conn) {
                    ui::set_away(hw(h), &me, away);
                }
            }
            // 341: the server confirms an invitation we sent
            let line = if code == 341 && params.len() >= 2 { format!("* Invited {} to {}", params[0], params[1]) } else { params.join(" ") };
            if !line.is_empty() {
                // a server that labels its answers sends them back to the window the command was typed in
                match v3::answer_window(conn) {
                    Some(h) => print(h, &line),
                    None => server_print(conn, &line),
                }
            }
        }
        Event::Error { text } => {
            // When we asked to quit, the server's "Closing Link ... (Client Quit)" is the normal goodbye, not an error.
            let expected = text.contains("Closing Link") && with(|a| a.conns.get(&conn).is_some_and(|c| c.want_quit));
            if let Some(h) = with(|a| a.conns.get(&conn).map(|c| c.server_win)).filter(|h| *h != 0) {
                if expected {
                    print_k(h, LineKind::Info, &format!("* The server closed the connection: {text}"));
                } else {
                    print_k(h, LineKind::Error, &format!("* Error: {text}"));
                }
            }
        }
        Event::Disconnected { reason } => {
            // Which channels were open, for a possible reconnect (read before the flags change).
            let was_registered = with(|a| a.conns.get(&conn).is_some_and(|c| c.registered));
            if was_registered {
                play_sound("disconnected", "");
                scripts::event(conn, None, "", "disconnect", &[]);
            }
            let channels = if was_registered { open_channels(conn) } else { Vec::new() };
            with(|a| {
                if let Some(c) = a.conns.get_mut(&conn) {
                    c.client = None;
                    c.registered = false;
                    c.presence.clear();
                    if was_registered {
                        c.rejoin = channels;
                    }
                }
            });
            refresh_buddies();
            for h in all_windows(conn) {
                print(h, &format!("* Disconnected ({reason})"));
            }
            let upgrade = with(|a| a.conns.get_mut(&conn).and_then(|c| c.sts_upgrade.take()));
            if let Some(port) = upgrade {
                sts_reconnect(conn, port);
                return;
            }
            schedule_reconnect(conn);
        }
    }
}
