//! The Channel List window: a modeless top-level window per connection with a search box,
//! a minimum-users filter, a virtual (owner-data) list view and Join / Refresh buttons.
//! Rows are fetched from the data model on demand, so huge `LIST` replies stay responsive.

use crate::app;
use crate::chanlist::{ChannelList, Column, Entry};
use crate::ui;
use crate::util::*;
use std::cell::RefCell;
use std::ffi::c_void;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("ChimeraIRC.ChannelList");
const ID_SEARCH: usize = 20;
const ID_MIN: usize = 21;
const ID_REFRESH: usize = 22;
const ID_JOIN: usize = 23;
const ID_LIST: usize = 24;
const TIMER: usize = 1;
/// Joining more channels than this in one go is almost certainly a mis-selected "select all".
const MAX_JOIN: usize = 10;

static REGISTERED: std::sync::Once = std::sync::Once::new();

thread_local! {
    /// Open list windows, so the main loop can give them dialog-style keyboard handling.
    static WINDOWS: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
}

struct State {
    conn: u32,
    l_search: HWND,
    l_min: HWND,
    list: HWND,
    search: HWND,
    min: HWND,
    status: HWND,
    data: ChannelList,
    loading: bool,
    dirty: bool,
}

fn state<'a>(h: HWND) -> Option<&'a mut State> {
    let p = unsafe { GetWindowLongPtrW(h, GWLP_USERDATA) } as *mut State;
    if p.is_null() {
        None
    } else {
        Some(unsafe { &mut *p })
    }
}

/// Tab / Enter / arrow handling for the open list windows; true if the message was consumed.
pub fn is_dialog_message(msg: &MSG) -> bool {
    let wins = WINDOWS.with(|w| w.borrow().clone());
    wins.into_iter().any(|h| unsafe { IsDialogMessageW(hw(h), msg) }.as_bool())
}

pub fn create(owner: HWND, conn: u32, host: &str) -> HWND {
    REGISTERED.call_once(|| unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: ui::hinstance(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: LoadIconW(ui::hinstance(), PCWSTR(1 as *const u16)).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as usize as *mut c_void),
            ..Default::default()
        };
        RegisterClassW(&wc);
    });
    let title = format!("Channel List - {host}");
    let st = Box::into_raw(Box::new(State {
        conn,
        l_search: HWND::default(),
        l_min: HWND::default(),
        list: HWND::default(),
        search: HWND::default(),
        min: HWND::default(),
        status: HWND::default(),
        data: ChannelList::default(),
        loading: false,
        dirty: false,
    }));
    let t = wide(&title);
    let h = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0), CLASS, PCWSTR(t.as_ptr()), WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT, CW_USEDEFAULT, px(780), px(540), owner, None, ui::hinstance(), Some(st as *const c_void),
        )
    };
    match h {
        Ok(h) => {
            WINDOWS.with(|w| w.borrow_mut().push(raw(h)));
            unsafe {
                let _ = ShowWindow(h, SW_SHOW);
                let _ = SetFocus(state(h).map(|s| s.search).unwrap_or_default());
            }
            h
        }
        Err(_) => {
            drop(unsafe { Box::from_raw(st) });
            HWND::default()
        }
    }
}

pub fn raise(h: HWND) {
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
    }
}

/// A new `LIST` reply is starting: forget the old rows and start receiving.
pub fn begin(h: HWND) {
    if let Some(s) = state(h) {
        s.data.clear();
        s.loading = true;
        s.dirty = true;
        unsafe {
            SetTimer(h, TIMER, 250, None);
        }
        refresh(h);
    }
}

pub fn push(h: HWND, e: Entry) {
    if let Some(s) = state(h) {
        s.data.push(e);
        s.dirty = true;
    }
}

pub fn finish(h: HWND) {
    if let Some(s) = state(h) {
        s.loading = false;
        unsafe {
            let _ = KillTimer(h, TIMER);
        }
    }
    refresh(h);
}

pub fn set_search(h: HWND, text: &str) {
    if let Some(s) = state(h) {
        let t = wide(text);
        unsafe {
            let _ = SetWindowTextW(s.search, PCWSTR(t.as_ptr()));
        }
    }
}

fn send(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    unsafe { SendMessageW(h, msg, WPARAM(wp), LPARAM(lp)).0 }
}

fn edit_text(h: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(h) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(h, &mut buf) as usize;
        String::from_utf16_lossy(&buf[..got])
    }
}

fn set_header_text(s: &State) {
    let (sorted, asc) = s.data.sort();
    for (i, (col, name)) in [(Column::Name, "Channel"), (Column::Users, "Users"), (Column::Topic, "Topic")].iter().enumerate() {
        let arrow = if *col == sorted { if asc { "  \u{25b2}" } else { "  \u{25bc}" } } else { "" };
        let mut t = wide(&format!("{name}{arrow}"));
        let mut c = LVCOLUMNW { mask: LVCF_TEXT, pszText: PWSTR(t.as_mut_ptr()), ..Default::default() };
        send(s.list, LVM_SETCOLUMNW, i, &mut c as *mut _ as isize);
    }
}

/// Re-apply the filter and sort, then update the list and the status line.
fn refresh(h: HWND) {
    let Some(s) = state(h) else { return };
    let min = edit_text(s.min).trim().parse::<u32>().unwrap_or(0);
    s.data.set_filter(&edit_text(s.search), min);
    s.data.rebuild();
    s.dirty = false;
    // LVSICF_NOSCROLL keeps the scroll position while rows keep arriving.
    send(s.list, LVM_SETITEMCOUNT, s.data.visible(), LVSICF_NOSCROLL as isize);
    unsafe {
        let _ = InvalidateRect(s.list, None, false);
    }
    set_header_text(s);
    let text = if s.loading {
        format!("Receiving channels... {} so far ({} shown)", s.data.total(), s.data.visible())
    } else {
        format!("{} of {} channels shown", s.data.visible(), s.data.total())
    };
    let t = wide(&text);
    unsafe {
        let _ = SetWindowTextW(s.status, PCWSTR(t.as_ptr()));
    }
}

/// Join every selected row (a handful at most).
fn join_selected(h: HWND) {
    let Some(s) = state(h) else { return };
    let mut names = Vec::new();
    let mut i = -1isize;
    loop {
        i = send(s.list, LVM_GETNEXTITEM, i as usize, LVNI_SELECTED as isize);
        if i < 0 {
            break;
        }
        if let Some(e) = s.data.get(i as usize) {
            names.push(e.name.clone());
        }
        if names.len() > MAX_JOIN {
            break;
        }
    }
    if names.len() > MAX_JOIN {
        crate::dialogs::message(h, "Channel List", &format!("Select at most {MAX_JOIN} channels to join at once."));
        return;
    }
    for n in names {
        app::join_channel(s.conn, &n);
    }
}

unsafe fn create_controls(hwnd: HWND, s: &mut State) {
    let hi = ui::hinstance();
    let font = ui::ui_font_raw() as usize;
    let make = |class: PCWSTR, text: &str, style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, id: usize| {
        let t = wide(text);
        let c = CreateWindowExW(
            ex, class, PCWSTR(t.as_ptr()), WS_CHILD | WS_VISIBLE | style, 0, 0, 0, 0, hwnd,
            HMENU(id as *mut c_void), hi, None,
        )
        .unwrap_or_default();
        SendMessageW(c, WM_SETFONT, WPARAM(font), LPARAM(1));
        c
    };
    s.l_search = make(w!("STATIC"), "Search:", WINDOW_STYLE(0x200), WINDOW_EX_STYLE(0), 0); // SS_CENTERIMAGE
    s.search = make(w!("EDIT"), "", WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32), WS_EX_CLIENTEDGE, ID_SEARCH);
    s.l_min = make(w!("STATIC"), "Min users:", WINDOW_STYLE(0x200), WINDOW_EX_STYLE(0), 0);
    s.min = make(w!("EDIT"), "", WS_TABSTOP | WINDOW_STYLE((ES_AUTOHSCROLL | ES_NUMBER) as u32), WS_EX_CLIENTEDGE, ID_MIN);
    make(w!("BUTTON"), "Refresh", WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32), WINDOW_EX_STYLE(0), ID_REFRESH);
    make(w!("BUTTON"), "Join", WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32), WINDOW_EX_STYLE(0), ID_JOIN);
    s.list = make(
        WC_LISTVIEWW,
        "",
        WS_TABSTOP | WINDOW_STYLE(LVS_REPORT | LVS_OWNERDATA | LVS_SHOWSELALWAYS),
        WS_EX_CLIENTEDGE,
        ID_LIST,
    );
    s.status = make(w!("STATIC"), "", WINDOW_STYLE(0x200), WINDOW_EX_STYLE(0), 0);
    send(
        s.list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        0,
        (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_HEADERDRAGDROP) as isize,
    );
    for (i, (name, width, right)) in [("Channel", 170, false), ("Users", 70, true), ("Topic", 480, false)].iter().enumerate() {
        let mut t = wide(name);
        let mut c = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH | LVCF_FMT | LVCF_SUBITEM,
            fmt: if *right { LVCFMT_RIGHT } else { LVCFMT_LEFT },
            cx: px(*width),
            pszText: PWSTR(t.as_mut_ptr()),
            iSubItem: i as i32,
            ..Default::default()
        };
        send(s.list, LVM_INSERTCOLUMNW, i, &mut c as *mut _ as isize);
    }
    if let Some(w) = app::saved_columns("chanlist", 3) {
        crate::listcols::apply(s.list, &w);
    }
}

unsafe fn layout(hwnd: HWND) {
    let Some(s) = state(hwnd) else { return };
    if s.list.0.is_null() {
        return;
    }
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right, rc.bottom);
    let (m, row, gap) = (px(8), px(24), px(6));
    let child = |id: usize| GetDlgItem(hwnd, id as i32).unwrap_or_default();
    let bw = px(80);
    let mut x = m;
    let _ = MoveWindow(s.l_search, x, m, px(50), row, true);
    x += px(50);
    let search_w = (w - x - px(70) - px(80) - bw * 2 - gap * 5 - m).max(px(80));
    let _ = MoveWindow(s.search, x, m, search_w, row, true);
    x += search_w + gap;
    let _ = MoveWindow(s.l_min, x, m, px(70), row, true);
    x += px(70);
    let _ = MoveWindow(s.min, x, m, px(60), row, true);
    let _ = MoveWindow(child(ID_REFRESH), w - m - bw * 2 - gap, m, bw, row, true);
    let _ = MoveWindow(child(ID_JOIN), w - m - bw, m, bw, row, true);
    let top = m + row + gap;
    let sh = px(20);
    let _ = MoveWindow(s.list, m, top, w - m * 2, (h - top - sh - m).max(0), true);
    let _ = MoveWindow(s.status, m, h - sh - m / 2, w - m * 2, sh, true);
}

extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                let cs = &*(lp.0 as *const CREATESTRUCTW);
                let st = cs.lpCreateParams as *mut State;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, st as isize);
                create_controls(hwnd, &mut *st);
                layout(hwnd);
                crate::look::style_popup(hwnd);
                LRESULT(0)
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => crate::look::dialog_colors(msg, wp),
            WM_SIZE => {
                layout(hwnd);
                LRESULT(0)
            }
            WM_GETMINMAXINFO => {
                let mm = &mut *(lp.0 as *mut MINMAXINFO);
                mm.ptMinTrackSize.x = px(520);
                mm.ptMinTrackSize.y = px(260);
                LRESULT(0)
            }
            WM_COMMAND => {
                let (id, note) = (wp.0 & 0xffff, (wp.0 >> 16) as u32);
                match id {
                    ID_SEARCH | ID_MIN if note == EN_CHANGE => refresh(hwnd),
                    ID_REFRESH => app::request_channel_list(state(hwnd).map_or(0, |s| s.conn)),
                    ID_JOIN => join_selected(hwnd),
                    id if id == IDCANCEL.0 as usize => {
                        let _ = DestroyWindow(hwnd); // Esc closes, like a dialog
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_NOTIFY => {
                let nm = &*(lp.0 as *const NMHDR);
                if nm.idFrom != ID_LIST {
                    return LRESULT(0);
                }
                let Some(s) = state(hwnd) else { return LRESULT(0) };
                match nm.code {
                    LVN_GETDISPINFOW => {
                        let di = &mut *(lp.0 as *mut NMLVDISPINFOW);
                        if di.item.mask.0 & LVIF_TEXT.0 != 0 && !di.item.pszText.0.is_null() && di.item.cchTextMax > 0 {
                            let text = match (s.data.get(di.item.iItem as usize), di.item.iSubItem) {
                                (Some(e), 0) => e.name.clone(),
                                (Some(e), 1) => e.users.to_string(),
                                (Some(e), _) => e.topic.clone(),
                                (None, _) => String::new(),
                            };
                            let units: Vec<u16> = text.encode_utf16().take(di.item.cchTextMax as usize - 1).collect();
                            let buf = std::slice::from_raw_parts_mut(di.item.pszText.0, di.item.cchTextMax as usize);
                            buf[..units.len()].copy_from_slice(&units);
                            buf[units.len()] = 0;
                        }
                    }
                    LVN_COLUMNCLICK => {
                        let nl = &*(lp.0 as *const NMLISTVIEW);
                        let col = match nl.iSubItem {
                            0 => Column::Name,
                            1 => Column::Users,
                            _ => Column::Topic,
                        };
                        s.data.sort_by(col);
                        refresh(hwnd);
                    }
                    NM_DBLCLK => join_selected(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_TIMER => {
                if wp.0 == TIMER && state(hwnd).is_some_and(|s| s.dirty) {
                    refresh(hwnd);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                if let Some(s) = state(hwnd) {
                    app::remember_columns("chanlist", &crate::listcols::read(s.list, 3));
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_NCDESTROY => {
                WINDOWS.with(|w| w.borrow_mut().retain(|x| *x != raw(hwnd)));
                let p = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut State;
                if !p.is_null() {
                    let conn = (*p).conn;
                    drop(Box::from_raw(p));
                    app::channel_list_closed(conn, raw(hwnd));
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
