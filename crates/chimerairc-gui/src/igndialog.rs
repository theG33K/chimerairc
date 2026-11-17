//! The Ignore List editor: entries with Add / Edit / Remove / Enable-Disable. Each entry is
//! edited with the generic form dialog.

use crate::dialogs::{self, Field, ID_CANCEL, ID_OK};
use crate::favorites;
use crate::ignorelist::Entry;
use crate::ui;
use crate::util::*;
use std::ffi::c_void;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use chimerairc_proto::mask;

const ID_ADD: usize = 10;
const ID_EDIT: usize = 11;
const ID_REMOVE: usize = 12;
const ID_TOGGLE: usize = 13;
const ID_LIST: usize = 14;
const CLASS: PCWSTR = w!("ChimeraIRC.IgnoreList");
static REGISTERED: std::sync::Once = std::sync::Once::new();

struct State {
    dlg: HWND,
    list: HWND,
    entries: Vec<Entry>,
    result: Option<Vec<Entry>>,
    done: bool,
}

/// Ask for one entry with the form dialog; `None` if cancelled. `initial` pre-fills it.
fn edit_entry(owner: HWND, initial: Option<&Entry>) -> Option<Entry> {
    let blank = Entry::new("x");
    let e = initial.unwrap_or(&blank);
    let (mask_text, notes, chans, net) = if initial.is_some() {
        (e.rule.mask.clone(), e.notes.clone(), e.rule.channels.join(", "), e.rule.network.clone().unwrap_or_default())
    } else {
        (String::new(), String::new(), String::new(), String::new())
    };
    let mut fields = vec![
        Field::text("Nick or mask", &mask_text),
        Field::text("Notes (optional)", &notes),
        Field::text("Only in channels", &chans),
        Field::text("Only on server", &net),
        Field::wide_check("Ignore messages and actions", e.rule.messages),
        Field::wide_check("Ignore notices", e.rule.notices),
        Field::wide_check("Ignore CTCP requests and replies", e.rule.ctcp),
    ];
    let title = if initial.is_some() { "Edit ignore" } else { "Add ignore" };
    loop {
        let v = dialogs::show(owner, title, &fields)?;
        let text = v[0].trim();
        let channels: Vec<String> =
            v[2].split(|c: char| c == ',' || c.is_whitespace()).filter(|c| !c.is_empty()).map(str::to_owned).collect();
        let network = v[3].trim();
        let problem: Option<String> = if text.is_empty() || text.contains(char::is_whitespace) {
            Some("Enter a nick or a mask such as troll or *!*@*.example.net (no spaces).".to_owned())
        } else if v[4] != "1" && v[5] != "1" && v[6] != "1" {
            Some("Tick at least one thing to ignore.".to_owned())
        } else if let Some(bad) = channels.iter().find(|c| !favorites::valid_channel(c)) {
            Some(format!("\"{bad}\" is not a channel name. Use names like #chimerairc, separated by commas."))
        } else if !network.is_empty() && !favorites::valid_host(network) {
            Some("Enter the server's host name, or leave it empty for every server.".to_owned())
        } else {
            None
        };
        if let Some(msg) = problem {
            dialogs::message(owner, "Ignore", &msg);
            for (f, val) in fields.iter_mut().zip(&v) {
                f.value = val.clone();
            }
            continue;
        }
        let mut out = Entry::new(text);
        out.rule.mask = mask::normalize(text);
        out.notes = v[1].trim().to_owned();
        out.rule.channels = channels;
        out.rule.network = (!network.is_empty()).then(|| network.to_owned());
        out.rule.messages = v[4] == "1";
        out.rule.notices = v[5] == "1";
        out.rule.ctcp = v[6] == "1";
        out.enabled = initial.is_none_or(|i| i.enabled);
        return Some(out);
    }
}

unsafe fn refresh(st: &State, select: Option<usize>) {
    SendMessageW(st.list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for e in &st.entries {
        let t = wide(&e.describe());
        SendMessageW(st.list, LB_ADDSTRING, WPARAM(0), LPARAM(t.as_ptr() as isize));
    }
    if let Some(i) = select.filter(|i| *i < st.entries.len()) {
        SendMessageW(st.list, LB_SETCURSEL, WPARAM(i), LPARAM(0));
    }
}

unsafe fn selected(st: &State) -> Option<usize> {
    let i = SendMessageW(st.list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    (i >= 0 && (i as usize) < st.entries.len()).then_some(i as usize)
}

unsafe fn on_edit(st: &mut State) {
    if let Some(i) = selected(st) {
        let current = st.entries[i].clone();
        if let Some(e) = edit_entry(st.dlg, Some(&current)) {
            st.entries[i] = e;
            refresh(st, Some(i));
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
                    ID_ADD => {
                        if let Some(e) = edit_entry(st.dlg, None) {
                            if crate::ignorelist::add(&mut st.entries, e) {
                                let last = st.entries.len() - 1;
                                refresh(st, Some(last));
                            } else {
                                dialogs::message(st.dlg, "Ignore", "That mask is already ignored with the same scope.");
                            }
                        }
                    }
                    ID_EDIT => on_edit(st),
                    ID_LIST if note == LBN_DBLCLK => on_edit(st),
                    ID_TOGGLE => {
                        if let Some(i) = selected(st) {
                            st.entries[i].enabled = !st.entries[i].enabled;
                            refresh(st, Some(i));
                        }
                    }
                    ID_REMOVE => {
                        if let Some(i) = selected(st) {
                            st.entries.remove(i);
                            refresh(st, Some(i.min(st.entries.len().saturating_sub(1))));
                        }
                    }
                    ID_OK => {
                        st.result = Some(st.entries.clone());
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
            // labels, buttons, text boxes and lists in the colours of the theme
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => crate::look::dialog_colors(msg, wp),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Modal ignore-list editor. Returns the edited list on OK, `None` on Cancel.
pub fn show(owner: HWND, current: &[Entry]) -> Option<Vec<Entry>> {
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
        let (m, list_w, list_h, btn_w, gap) = (px(14), px(430), px(250), px(116), px(10));
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
        let title = wide("Ignore List");
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
        make(w!("STATIC"), "People whose messages are hidden:", WINDOW_STYLE(0), WINDOW_EX_STYLE(0), m, m, list_w, px(18), 0);
        let list = make(
            w!("LISTBOX"),
            "",
            WS_TABSTOP | WS_VSCROLL | WS_HSCROLL | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32),
            WS_EX_CLIENTEDGE,
            m,
            m + px(18),
            list_w,
            list_h,
            ID_LIST,
        );
        SendMessageW(list, LB_SETHORIZONTALEXTENT, WPARAM(px(900) as usize), LPARAM(0));
        let bx = m + list_w + gap;
        let by = m + px(18);
        for (i, (text, id)) in
            [("Add...", ID_ADD), ("Edit...", ID_EDIT), ("Remove", ID_REMOVE), ("Enable / Disable", ID_TOGGLE)].iter().enumerate()
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

        let mut state = Box::new(State { dlg, list, entries: current.to_vec(), result: None, done: false });
        refresh(&state, if state.entries.is_empty() { None } else { Some(0) });
        SetWindowLongPtrW(dlg, GWLP_USERDATA, &mut *state as *mut State as isize);
        crate::look::style_popup(dlg);
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
