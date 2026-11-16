//! Organize Favorites: a list of favorite servers (with their channels indented beneath) and
//! Add Server / Add Channel / Edit / Remove. Each item is edited with the generic form dialog.

use crate::dialogs::{self, Field, ID_CANCEL, ID_OK};
use crate::favorites::{self, Favorites, Server};
use crate::ui;
use crate::util::*;
use std::ffi::c_void;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

const ID_ADD_SERVER: usize = 10;
const ID_ADD_CHANNEL: usize = 11;
const ID_EDIT: usize = 12;
const ID_REMOVE: usize = 13;
const ID_LIST: usize = 14;
const CLASS: PCWSTR = w!("ChimeraIRC.Favorites");
static REGISTERED: std::sync::Once = std::sync::Once::new();

/// A row of the list: a server, or one of its channels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Row {
    server: usize,
    channel: Option<usize>,
}

struct State {
    dlg: HWND,
    list: HWND,
    favs: Favorites,
    rows: Vec<Row>,
    result: Option<Favorites>,
    done: bool,
}

fn build_rows(f: &Favorites) -> Vec<Row> {
    let mut rows = Vec::new();
    for (si, s) in f.servers.iter().enumerate() {
        rows.push(Row { server: si, channel: None });
        rows.extend((0..s.channels.len()).map(|ci| Row { server: si, channel: Some(ci) }));
    }
    rows
}

fn describe(f: &Favorites, r: Row) -> String {
    let s = &f.servers[r.server];
    match r.channel {
        None => format!("{}   \u{2014}   {}:{} {}", s.label(), s.host, s.port, if s.tls { "(TLS)" } else { "(plaintext)" }),
        Some(ci) => format!("        {}", s.channels[ci]),
    }
}

unsafe fn refresh(st: &mut State, select: Option<Row>) {
    st.rows = build_rows(&st.favs);
    SendMessageW(st.list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for r in &st.rows {
        let t = wide(&describe(&st.favs, *r));
        SendMessageW(st.list, LB_ADDSTRING, WPARAM(0), LPARAM(t.as_ptr() as isize));
    }
    if let Some(i) = select.and_then(|s| st.rows.iter().position(|r| *r == s)) {
        SendMessageW(st.list, LB_SETCURSEL, WPARAM(i), LPARAM(0));
    }
}

unsafe fn selected(st: &State) -> Option<Row> {
    let i = SendMessageW(st.list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    (i >= 0).then(|| st.rows.get(i as usize).copied()).flatten()
}

/// Form for a server favorite; `None` if cancelled. `initial` pre-fills it for editing.
fn server_form(owner: HWND, initial: Option<&Server>) -> Option<Server> {
    let (name, host, port, tls, chans) = match initial {
        Some(s) => (s.name.clone(), s.host.clone(), s.port.to_string(), s.tls, s.channels.join(", ")),
        None => (String::new(), String::new(), "6697".to_owned(), true, String::new()),
    };
    let mut fields = vec![
        Field::text("Name (optional)", &name),
        Field::text("Server", &host),
        Field::number("Port", &port),
        Field::check("Use TLS (recommended)", tls),
        Field::text("Channels to join", &chans),
    ];
    let title = if initial.is_some() { "Edit favorite server" } else { "Add favorite server" };
    loop {
        let v = dialogs::show(owner, title, &fields)?;
        let port = v[2].trim().parse::<u16>().ok().filter(|p| *p > 0);
        let problem = if !favorites::valid_host(v[1].trim()) {
            Some("Enter the server's host name (no spaces).")
        } else if port.is_none() {
            Some("Port must be a number from 1 to 65535.")
        } else {
            None
        };
        if let Some(msg) = problem {
            dialogs::message(owner, "Favorites", msg);
            for (f, val) in fields.iter_mut().zip(&v) {
                f.value = val.clone();
            }
            continue;
        }
        let chans: Vec<String> = v[4]
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|c| !c.is_empty())
            .map(|c| if c.starts_with(['#', '&', '+', '!']) { c.to_owned() } else { format!("#{c}") })
            .collect();
        return Some(Server::new(&v[0], v[1].trim(), port?, v[3] == "1", &chans));
    }
}

/// Form for a channel name; `None` if cancelled.
fn channel_form(owner: HWND, initial: &str) -> Option<String> {
    let mut value = initial.to_owned();
    loop {
        let fields = [Field::text("Channel", if value.is_empty() { "#" } else { &value })];
        let v = dialogs::show(owner, if initial.is_empty() { "Add favorite channel" } else { "Edit favorite channel" }, &fields)?;
        let c = v[0].trim();
        if favorites::valid_channel(c) {
            return Some(c.to_owned());
        }
        dialogs::message(owner, "Favorites", "Enter a channel name such as #chimerairc (no spaces or commas).");
        value = v[0].clone();
    }
}

unsafe fn on_add_server(st: &mut State) {
    if let Some(s) = server_form(st.dlg, None) {
        let host = s.host.clone();
        let port = s.port;
        if st.favs.add_server(s) {
            let si = st.favs.find(&host, port).unwrap_or(0);
            refresh(st, Some(Row { server: si, channel: None }));
        } else {
            dialogs::message(st.dlg, "Favorites", "That server and port is already in your favorites.");
        }
    }
}

unsafe fn on_add_channel(st: &mut State) {
    // The channel goes on the selected row's server (or the only server, if there is just one).
    let server = match selected(st) {
        Some(r) => r.server,
        None if st.favs.servers.len() == 1 => 0,
        None => {
            dialogs::message(st.dlg, "Favorites", "Select a server first (or add one with Add Server).");
            return;
        }
    };
    if let Some(c) = channel_form(st.dlg, "") {
        if st.favs.servers[server].add_channel(&c) {
            let ci = st.favs.servers[server].channels.len() - 1;
            refresh(st, Some(Row { server, channel: Some(ci) }));
        } else {
            dialogs::message(st.dlg, "Favorites", "That channel is already in this server's favorites.");
        }
    }
}

unsafe fn on_edit(st: &mut State) {
    let Some(r) = selected(st) else { return };
    match r.channel {
        None => {
            let current = st.favs.servers[r.server].clone();
            if let Some(new) = server_form(st.dlg, Some(&current)) {
                // Changing host/port must not collide with a different favorite.
                let clash = st
                    .favs
                    .servers
                    .iter()
                    .enumerate()
                    .any(|(i, s)| i != r.server && s.same_endpoint(&new.host, new.port));
                if clash {
                    dialogs::message(st.dlg, "Favorites", "Another favorite already uses that server and port.");
                } else {
                    st.favs.servers[r.server] = new;
                    refresh(st, Some(Row { server: r.server, channel: None }));
                }
            }
        }
        Some(ci) => {
            let current = st.favs.servers[r.server].channels[ci].clone();
            if let Some(new) = channel_form(st.dlg, &current) {
                let dup = st.favs.servers[r.server]
                    .channels
                    .iter()
                    .enumerate()
                    .any(|(i, c)| i != ci && c.eq_ignore_ascii_case(&new));
                if dup {
                    dialogs::message(st.dlg, "Favorites", "That channel is already in this server's favorites.");
                } else {
                    st.favs.servers[r.server].channels[ci] = new;
                    refresh(st, Some(r));
                }
            }
        }
    }
}

unsafe fn on_remove(st: &mut State) {
    let Some(r) = selected(st) else { return };
    match r.channel {
        Some(ci) => {
            st.favs.servers[r.server].channels.remove(ci);
            let keep = if ci > 0 { Row { server: r.server, channel: Some(ci - 1) } } else { Row { server: r.server, channel: None } };
            refresh(st, Some(keep));
        }
        None => {
            let s = &st.favs.servers[r.server];
            let n = s.channels.len();
            let msg = format!("Remove {} from your favorites{}?", s.label(), if n > 0 { format!(", along with its {n} channel(s)") } else { String::new() });
            if dialogs::confirm(st.dlg, "Remove favorite", &msg) {
                st.favs.servers.remove(r.server);
                let keep = st.favs.servers.len().checked_sub(1).map(|last| Row { server: r.server.min(last), channel: None });
                refresh(st, keep);
            }
        }
    }
}

extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        match msg {
            WM_COMMAND if !st.is_null() => {
                let st = &mut *st;
                let (id, note) = (wp.0 & 0xffff, (wp.0 >> 16) as u32);
                match id {
                    ID_ADD_SERVER => on_add_server(st),
                    ID_ADD_CHANNEL => on_add_channel(st),
                    ID_EDIT => on_edit(st),
                    ID_LIST if note == LBN_DBLCLK => on_edit(st),
                    ID_REMOVE => on_remove(st),
                    ID_OK => {
                        st.result = Some(st.favs.clone());
                        st.done = true;
                    }
                    ID_CANCEL => st.done = true,
                    _ => {}
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                if !st.is_null() {
                    (*st).done = true;
                }
                LRESULT(0) // the modal loop destroys the window
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
                let dc = HDC(wp.0 as *mut c_void);
                SetBkColor(dc, windows::Win32::Foundation::COLORREF(GetSysColor(COLOR_BTNFACE)));
                LRESULT(GetSysColorBrush(COLOR_BTNFACE).0 as isize)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Modal editor. Returns the edited favorites on OK, `None` on Cancel.
pub fn show(owner: HWND, current: &Favorites) -> Option<Favorites> {
    unsafe {
        let hi = ui::hinstance();
        REGISTERED.call_once(|| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: hi,
                lpszClassName: CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as usize as *mut c_void),
                ..Default::default()
            };
            RegisterClassW(&wc);
        });
        let (m, list_w, list_h, btn_w, gap) = (px(14), px(390), px(250), px(116), px(10));
        let client_w = m * 2 + list_w + gap + btn_w;
        let client_h = m + px(18) + list_h + px(12) + px(26) + m;
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let ex = WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT;
        let mut rc = RECT { left: 0, top: 0, right: client_w, bottom: client_h };
        let _ = AdjustWindowRectEx(&mut rc, style, false, ex);
        let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
        let mut orc = RECT::default();
        let _ = GetWindowRect(owner, &mut orc);
        let x = (orc.left + (orc.right - orc.left - ww) / 2).max(0);
        let y = (orc.top + (orc.bottom - orc.top - wh) / 3).max(0);
        let title = wide("Organize Favorites");
        let dlg = CreateWindowExW(ex, CLASS, PCWSTR(title.as_ptr()), style, x, y, ww, wh, owner, None, hi, None).ok()?;
        let font = ui::ui_font_raw();

        let make = |class: PCWSTR, text: &str, st: WINDOW_STYLE, exs: WINDOW_EX_STYLE, x, y, w, h, id: usize| {
            let t = wide(text);
            let c = CreateWindowExW(
                exs, class, PCWSTR(t.as_ptr()), WS_CHILD | WS_VISIBLE | st, x, y, w, h, dlg,
                HMENU(id as *mut c_void), hi, None,
            )
            .unwrap_or_default();
            SendMessageW(c, WM_SETFONT, WPARAM(font as usize), LPARAM(1));
            c
        };
        make(w!("STATIC"), "Your favorite servers and channels:", WINDOW_STYLE(0), WINDOW_EX_STYLE(0), m, m, list_w, px(18), 0);
        let list = make(
            w!("LISTBOX"),
            "",
            WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32),
            WS_EX_CLIENTEDGE,
            m,
            m + px(18),
            list_w,
            list_h,
            ID_LIST,
        );
        let bx = m + list_w + gap;
        let by = m + px(18);
        for (i, (text, id)) in [("Add Server...", ID_ADD_SERVER), ("Add Channel...", ID_ADD_CHANNEL), ("Edit...", ID_EDIT), ("Remove", ID_REMOVE)]
            .iter()
            .enumerate()
        {
            make(
                w!("BUTTON"), text, WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32), WINDOW_EX_STYLE(0),
                bx, by + px(32) * i as i32, btn_w, px(26), *id,
            );
        }
        let oy = client_h - m - px(26);
        let bw = px(84);
        make(
            w!("BUTTON"), "OK", WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32), WINDOW_EX_STYLE(0),
            client_w - m - bw * 2 - px(8), oy, bw, px(26), ID_OK,
        );
        make(
            w!("BUTTON"), "Cancel", WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32), WINDOW_EX_STYLE(0),
            client_w - m - bw, oy, bw, px(26), ID_CANCEL,
        );

        let mut state = Box::new(State { dlg, list, favs: current.clone(), rows: Vec::new(), result: None, done: false });
        let first = (!state.favs.servers.is_empty()).then_some(Row { server: 0, channel: None });
        refresh(&mut state, first);
        SetWindowLongPtrW(dlg, GWLP_USERDATA, &mut *state as *mut State as isize);
        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetFocus(list);

        let mut msg = MSG::default();
        while !state.done {
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                PostQuitMessage(0); // app is exiting; propagate
                break;
            }
            if !IsDialogMessageW(dlg, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let _ = EnableWindow(owner, true);
        SetWindowLongPtrW(dlg, GWLP_USERDATA, 0);
        let _ = DestroyWindow(dlg);
        let _ = SetFocus(owner);
        state.result.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_list_each_server_followed_by_its_channels() {
        let mut f = Favorites::default();
        f.add_server(Server::new("A", "a.example", 6697, true, &["#x".to_owned(), "#y".to_owned()]));
        f.add_server(Server::new("B", "b.example", 6667, false, &[]));
        let rows = build_rows(&f);
        assert_eq!(
            rows,
            [
                Row { server: 0, channel: None },
                Row { server: 0, channel: Some(0) },
                Row { server: 0, channel: Some(1) },
                Row { server: 1, channel: None },
            ]
        );
        assert!(describe(&f, rows[0]).contains("a.example:6697 (TLS)"));
        assert!(describe(&f, rows[3]).contains("(plaintext)"));
        assert!(describe(&f, rows[1]).trim() == "#x");
    }
}
