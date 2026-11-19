//! The program's side of scripting: loading the files, telling the scripts what happens, and carrying out what they
//! ask for. The scripting itself is in `script.rs`.

use super::*;
use crate::script::{self, Action, Ctx, Host, Val};
use std::cell::Cell;
use std::sync::OnceLock;
use std::time::Instant;

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
    /// While a line a script asked for is being run, scripts are not asked about it again (no endless loops).
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

static START: OnceLock<Instant> = OnceLock::new();

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// `%AppData%\ChimeraIRC\scripts`.
pub(super) fn dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|b| PathBuf::from(b).join("ChimeraIRC").join("scripts"))
}

fn ctx_for(conn: u32, target: &str) -> Ctx {
    let (nick, network) = with(|a| a.conns.get(&conn).map(|c| (c.nick.clone(), c.host.clone())).unwrap_or_default());
    Ctx { nick, target: target.to_owned(), network }
}

/// The window answers go to: the given one, or the connection's status window, or the active window.
fn window_or_default(win: Option<isize>, conn: u32) -> Option<isize> {
    win.filter(|h| *h != 0)
        .or_else(|| with(|a| a.conns.get(&conn).map(|c| c.server_win)).filter(|h| *h != 0))
        .or_else(|| ui::active_child().map(|h| raw(h)))
}

/// Carry out what scripts asked for, in the window `h`.
fn run(h: Option<isize>, actions: Vec<Action>) {
    let Some(h) = h else { return };
    for a in actions {
        match a {
            Action::Echo(text) => print(h, &text),
            Action::Run(line) => {
                DEPTH.with(|d| d.set(d.get() + 1));
                submit(hw(h), line);
                DEPTH.with(|d| d.set(d.get() - 1));
            }
        }
    }
}

fn with_host<R>(f: impl FnOnce(&mut Host) -> R) -> Option<R> {
    HOST.with(|c| c.borrow_mut().as_mut().map(f))
}

fn active_window() -> Option<isize> {
    ui::active_child().map(|h| raw(h))
}

/// Make the scripts folder (with an example the first time) and load every `.lua` file in it.
pub(super) fn start() {
    let Some(dir) = dir() else { return };
    let first = !dir.exists();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if first {
        let _ = std::fs::write(dir.join("example.lua.off"), script::EXAMPLE);
    }
    let Ok(mut host) = Host::new(Some(&dir)) else { return };
    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some((script::script_name(&e.file_name().to_string_lossy())?, e.path())))
        .collect();
    files.sort();
    let (mut pending, mut loaded) = (Vec::new(), 0usize);
    for (name, path) in files {
        match std::fs::metadata(&path) {
            Ok(m) if m.len() <= 1 << 20 => {}
            _ => {
                pending.push(Action::Echo(format!("* [script {name}] skipped: the file is missing or larger than 1 MB.")));
                continue;
            }
        }
        match std::fs::read_to_string(&path) {
            Ok(src) => {
                pending.extend(host.load_source(&name, &src, now_ms(), &Ctx::default()));
                loaded = host.names().len();
            }
            Err(e) => pending.push(Action::Echo(format!("* [script {name}] could not be read: {e}"))),
        }
    }
    if loaded > 0 {
        pending.insert(0, Action::Echo(format!("* Scripts loaded: {}.", host.names().join(", "))));
    }
    HOST.with(|c| *c.borrow_mut() = Some(host));
    run(active_window(), pending);
}

/// Load everything again (new state: what scripts had set up is forgotten, their saved values stay).
pub(super) fn reload(h: isize) {
    HOST.with(|c| *c.borrow_mut() = None);
    start();
    let n = with_host(|host| host.names().len()).unwrap_or(0);
    if n == 0 {
        print(h, "* No scripts loaded. Put .lua files in the scripts folder (Options > Open Scripts Folder).");
    }
}

/// An event for the scripts: `message`, `notice`, `join`, `part`, `connect`, `disconnect`.
pub(super) fn event(conn: u32, win: Option<isize>, target: &str, name: &str, fields: &[(&str, Val)]) {
    let ctx = ctx_for(conn, target);
    let Some(actions) = with_host(|h| h.fire(name, fields, now_ms(), &ctx)) else { return };
    if !actions.is_empty() {
        run(window_or_default(win, conn), actions);
    }
}

/// A chat message (or notice, or action) arrived.
#[allow(clippy::too_many_arguments)]
pub(super) fn message(conn: u32, nick: &str, target: &str, text: &str, kind: ChatKind, from_me: bool, msgid: Option<&str>, reply_to: Option<&str>) {
    if with_host(|h| h.names().is_empty()).unwrap_or(true) {
        return;
    }
    let private = !is_channel(target);
    let peer = if from_me { target } else { nick };
    let win = if private { window_for(conn, Kind::Query, peer, false) } else { window_for(conn, Kind::Channel, target, false) };
    let name = if kind == ChatKind::Notice { "notice" } else { "message" };
    let net = with(|a| a.conns.get(&conn).map(|c| c.host.clone())).unwrap_or_default();
    let fields = [
        ("nick", Val::from(nick)),
        ("target", Val::from(if private { peer } else { target })),
        ("text", Val::from(format::strip(text))),
        ("private", Val::from(private)),
        ("action", Val::from(kind == ChatKind::Action)),
        ("mine", Val::from(from_me)),
        ("network", Val::from(net)),
        ("msgid", Val::from(msgid.unwrap_or(""))),
        ("reply_to", Val::from(reply_to.unwrap_or(""))),
    ];
    event(conn, win, if private { peer } else { target }, name, &fields);
}

/// A line was typed. True if a script took it over (then it is not run or sent).
pub(super) fn input(h: HWND, kind: Kind, conn: u32, name: &str, text: &str) -> bool {
    if DEPTH.with(|d| d.get()) > 0 {
        return false;
    }
    let target = if matches!(kind, Kind::Channel | Kind::Query) { name } else { "" };
    let ctx = ctx_for(conn, target);
    let Some((taken, actions)) = with_host(|host| host.input(text, now_ms(), &ctx)) else { return false };
    run(Some(raw(h)), actions);
    taken
}

/// `/name args` for a command a script added. True if there is one.
pub(super) fn command(h: HWND, kind: Kind, conn: u32, name: &str, verb: &str, rest: &str) -> bool {
    let target = if matches!(kind, Kind::Channel | Kind::Query) { name } else { "" };
    let ctx = ctx_for(conn, target);
    match with_host(|host| host.command(verb, rest, now_ms(), &ctx)).flatten() {
        Some(actions) => {
            run(Some(raw(h)), actions);
            true
        }
        None => false,
    }
}

/// The once-a-second (or so) call that lets timers run.
pub(super) fn tick() {
    if !with_host(|h| h.has_timers()).unwrap_or(false) {
        return;
    }
    let win = active_window();
    let ctx = ui::active_child().and_then(|h| ui::child(h).map(|c| (c.conn, c.name.clone()))).map(|(conn, n)| ctx_for(conn, &n)).unwrap_or_default();
    if let Some(actions) = with_host(|h| h.tick(now_ms(), &ctx)) {
        run(win, actions);
    }
}

/// `/script` (list), `/script reload`, `/script unload <name>`, `/script folder`.
pub(super) fn command_script(h: HWND, rest: &str) {
    let (sub, arg) = rest.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((rest, ""));
    match sub.to_ascii_lowercase().as_str() {
        "" | "list" => {
            let names = with_host(|host| host.names().to_vec()).unwrap_or_default();
            if names.is_empty() {
                local(h, "* No scripts loaded. Put .lua files in the scripts folder (/script folder), then /script reload.");
            } else {
                local(h, &format!("* Scripts: {}.", names.join(", ")));
                let cmds = with_host(|host| host.command_names()).unwrap_or_default();
                if !cmds.is_empty() {
                    local(h, &format!("  Commands they added: {}.", cmds.iter().map(|c| format!("/{c}")).collect::<Vec<_>>().join(" ")));
                }
            }
        }
        "reload" => reload(raw(h)),
        "unload" => {
            if arg.is_empty() {
                local(h, "* Usage: /script unload <name>");
            } else if with_host(|host| host.unload(arg)).unwrap_or(false) {
                local(h, &format!("* Script {arg} unloaded."));
            } else {
                local(h, &format!("* No script named {arg} is loaded."));
            }
        }
        "folder" | "open" => open_folder(),
        _ => local(h, "* Usage: /script [list]   /script reload   /script unload <name>   /script folder"),
    }
}

pub(super) fn open_folder() {
    if let Some(d) = dir() {
        if std::fs::create_dir_all(&d).is_ok() {
            shell_open(&d);
        }
    }
}
