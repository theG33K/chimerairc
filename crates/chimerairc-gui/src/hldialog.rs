//! The Highlights editor: a list of rules with Add / Edit / Remove, plus the notification options.
//! Each rule is edited with the generic form dialog.

use crate::dialogs::{self, Field, ID_CANCEL, ID_OK};
use crate::highlight::{self, Rule};
use chimerairc_proto::format::Rgb;
use crate::ui;
use crate::util::*;
use std::ffi::c_void;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

/// The editable highlight settings.
#[derive(Clone)]
pub struct HighlightPrefs {
    pub nick: bool,
    pub flash: bool,
    pub rules: Vec<Rule>,
}

const ID_ADD: usize = 10;
const ID_EDIT: usize = 11;
const ID_REMOVE: usize = 12;
const ID_LIST: usize = 13;
const CLASS: PCWSTR = w!("ChimeraIRC.Highlights");
static REGISTERED: std::sync::Once = std::sync::Once::new();

struct State {
    dlg: HWND,
    list: HWND,
    checks: [HWND; 2],
    rules: Vec<Rule>,
    result: Option<HighlightPrefs>,
    done: bool,
}

/// Ask for one rule with the form dialog; `None` if cancelled.
fn edit_rule(owner: HWND, initial: Option<&Rule>) -> Option<Rule> {
    let blank = Rule { whole_word: true, ..Rule::new("", Rgb(0xc0, 0, 0)) };
    let r0 = initial.unwrap_or(&blank);
    let mut fields = vec![
        Field::text("Word or pattern", &r0.word),
        Field::wide_check("This is a regular expression", r0.regex),
        Field::wide_check("Whole words only (not for regular expressions)", r0.whole_word),
        Field::wide_check("Case sensitive", r0.case_sensitive),
        Field::wide_check("Colour the entire line", r0.whole_line),
        Field::text("Colour", &highlight::color_name(r0.color)),
        Field::text("Only in", &r0.places),
        Field::note("Only in: leave empty for everywhere, or list channels separated by spaces, with * and ? as wildcards, for example #rust #py* . Put ! before one to leave it out (!#offtopic), and write query for private chats."),
        Field::text("Sound", &r0.sound),
        Field::note("Sound: leave empty to use the highlight sound; or off, a Windows sound (asterisk, exclamation, hand, question, default, notify, message, mail, reminder, sms), or the path of a .wav file."),
    ];
    let title = if initial.is_some() { "Edit highlight" } else { "Add highlight" };
    loop {
        let v = dialogs::show(owner, title, &fields)?;
        let word = v[0].trim();
        let regex = v[1] == "1";
        let sound = v[8].trim();
        let problem = if word.is_empty() {
            Some("Enter the word or phrase to highlight.".to_owned())
        } else if highlight::parse_color(&v[5]).is_none() {
            Some("Colour must be a name (red, orange, gold, green, teal, blue, purple, pink, brown, grey, black) or a 6-digit hex value such as C00000.".to_owned())
        } else if let Some(e) = regex.then(|| highlight::compile(word, v[3] == "1").err()).flatten() {
            Some(format!("That is not a valid regular expression:\n\n{e}"))
        } else if let Some(e) = (!sound.is_empty()).then(|| crate::sounds::check(sound).err()).flatten() {
            Some(e)
        } else {
            None
        };
        if let Some(msg) = problem {
            dialogs::message(owner, "Highlight", &msg);
            for (f, val) in fields.iter_mut().zip(&v) {
                f.value = val.clone();
            }
            continue;
        }
        return Some(Rule {
            word: word.to_owned(),
            regex,
            whole_word: v[2] == "1",
            case_sensitive: v[3] == "1",
            whole_line: v[4] == "1",
            color: highlight::parse_color(&v[5])?,
            places: v[6].trim().to_owned(),
            sound: sound.to_owned(),
        });
    }
}

unsafe fn refresh_list(st: &State, select: Option<usize>) {
    SendMessageW(st.list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for r in &st.rules {
        let t = wide(&r.describe());
        SendMessageW(st.list, LB_ADDSTRING, WPARAM(0), LPARAM(t.as_ptr() as isize));
    }
    if let Some(i) = select.filter(|i| *i < st.rules.len()) {
        SendMessageW(st.list, LB_SETCURSEL, WPARAM(i), LPARAM(0));
    }
}

unsafe fn selected(st: &State) -> Option<usize> {
    let i = SendMessageW(st.list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    (i >= 0 && (i as usize) < st.rules.len()).then_some(i as usize)
}

unsafe fn edit_selected(st: &mut State) {
    if let Some(i) = selected(st) {
        let current = st.rules[i].clone();
        if let Some(r) = edit_rule(st.dlg, Some(&current)) {
            st.rules[i] = r;
            refresh_list(st, Some(i));
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
                        if let Some(r) = edit_rule(st.dlg, None) {
                            st.rules.push(r);
                            let last = st.rules.len() - 1;
                            refresh_list(st, Some(last));
                        }
                    }
                    ID_EDIT => edit_selected(st),
                    ID_LIST if note == LBN_DBLCLK => edit_selected(st),
                    ID_REMOVE => {
                        if let Some(i) = selected(st) {
                            st.rules.remove(i);
                            refresh_list(st, Some(i.min(st.rules.len().saturating_sub(1))));
                        }
                    }
                    ID_OK => {
                        let on = |h: HWND| SendMessageW(h, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == BST_CHECKED.0 as isize;
                        st.result = Some(HighlightPrefs {
                            nick: on(st.checks[0]),
                            flash: on(st.checks[1]),
                            rules: st.rules.clone(),
                        });
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

/// Modal highlights editor. Returns the new settings on OK, `None` on Cancel.
pub fn show(owner: HWND, current: &HighlightPrefs) -> Option<HighlightPrefs> {
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
        let (m, list_w, list_h, btn_w, gap, row) = (px(14), px(330), px(150), px(92), px(10), px(24));
        let client_w = m * 2 + list_w + gap + btn_w;
        let client_h = m + px(18) + list_h + px(10) + row * 2 + px(12) + px(26) + m;
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let ex = WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT;
        let mut rc = RECT { left: 0, top: 0, right: client_w, bottom: client_h };
        let _ = AdjustWindowRectEx(&mut rc, style, false, ex);
        let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
        let mut orc = RECT::default();
        let _ = GetWindowRect(owner, &mut orc);
        let x = (orc.left + (orc.right - orc.left - ww) / 2).max(0);
        let y = (orc.top + (orc.bottom - orc.top - wh) / 3).max(0);
        let title = wide("Highlights");
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
        make(w!("STATIC"), "Words and patterns that make a message stand out:", WINDOW_STYLE(0), WINDOW_EX_STYLE(0), m, m, list_w, px(18), 0);
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
        let button = |text: &str, id: usize, y: i32| {
            make(w!("BUTTON"), text, WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32), WINDOW_EX_STYLE(0), bx, y, btn_w, px(26), id)
        };
        button("Add...", ID_ADD, by);
        button("Edit...", ID_EDIT, by + px(32));
        button("Remove", ID_REMOVE, by + px(64));

        let cy = m + px(18) + list_h + px(10);
        let check = |text: &str, on: bool, i: i32| {
            let c = make(
                w!("BUTTON"), text, WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32), WINDOW_EX_STYLE(0),
                m, cy + row * i, list_w + gap + btn_w, row, 0,
            );
            if on {
                SendMessageW(c, BM_SETCHECK, WPARAM(BST_CHECKED.0 as usize), LPARAM(0));
            }
            c
        };
        let checks = [
            check("Highlight lines that mention my nick", current.nick, 0),
            check("Flash the taskbar when a highlight arrives in the background", current.flash, 1),
        ];
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

        let mut state = Box::new(State { dlg, list, checks, rules: current.rules.clone(), result: None, done: false });
        refresh_list(&state, if state.rules.is_empty() { None } else { Some(0) });
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
