//! The DCC Transfers window: a modeless list of file transfers and chats with their progress,
//! plus Cancel / Show in Folder / Clear Finished. It repaints from `app::dcc_rows()` on every
//! DCC event and twice a second while transfers move.

use crate::app;
use crate::ui;
use crate::util::*;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("ChimeraIRC.Transfers");
const ID_LIST: usize = 20;
const ID_CANCEL: usize = 21;
const ID_FOLDER: usize = 22;
const ID_CLEAR: usize = 23;
const ID_CLOSE: usize = 24;
const ID_LOG: usize = 25;
const ID_OPEN: usize = 26;
const TIMER: usize = 1;
static REGISTERED: std::sync::Once = std::sync::Once::new();

thread_local! {
    /// The one Transfers window (0 = closed) and the transfer id behind each row.
    static WINDOW: Cell<isize> = const { Cell::new(0) };
    static IDS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

fn child(h: HWND, id: usize) -> HWND {
    unsafe { GetDlgItem(h, id as i32).unwrap_or_default() }
}

fn send(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    unsafe { SendMessageW(h, msg, WPARAM(wp), LPARAM(lp)).0 }
}

/// Show the window, creating it if needed.
pub fn open(owner: HWND) {
    let existing = WINDOW.with(|w| w.get());
    if existing != 0 {
        let h = hw(existing);
        unsafe {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
        }
        return;
    }
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
    let title = wide("DCC Transfers");
    // Tuck it into the lower right of the main window so it does not cover the chat area.
    let (w, h_) = (px(820), px(340));
    let mut orc = RECT::default();
    unsafe {
        let _ = GetWindowRect(owner, &mut orc);
    }
    let x = (orc.right - w - px(24)).max(0);
    let y = (orc.bottom - h_ - px(48)).max(0);
    let h = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0), CLASS, PCWSTR(title.as_ptr()), WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            x, y, w, h_, owner, None, ui::hinstance(), None,
        )
    };
    if let Ok(h) = h {
        WINDOW.with(|w| w.set(raw(h)));
        unsafe {
            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
        }
        refresh();
    }
}

/// Repaint the list from the current transfers (does nothing if the window is closed).
pub fn refresh() {
    let wnd = WINDOW.with(|w| w.get());
    if wnd == 0 {
        return;
    }
    let (win, list) = (hw(wnd), child(hw(wnd), ID_LIST));
    if list.0.is_null() {
        return;
    }
    // Keep the selection across the rebuild.
    let selected_id = IDS.with(|ids| {
        let i = send(list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
        (i >= 0).then(|| ids.borrow().get(i as usize).copied()).flatten()
    });
    let rows = app::dcc_rows();
    unsafe {
        send(list, WM_SETREDRAW, 0, 0);
        send(list, LVM_DELETEALLITEMS, 0, 0);
        for (i, r) in rows.iter().enumerate() {
            let cols = [&r.peer, &r.name, &r.size, &r.progress, &r.speed, &r.status];
            let mut first = wide(&format!("{} - {}", r.direction, cols[0]));
            let mut item = LVITEMW { mask: LVIF_TEXT, iItem: i as i32, pszText: PWSTR(first.as_mut_ptr()), ..Default::default() };
            send(list, LVM_INSERTITEMW, 0, &mut item as *mut _ as isize);
            for (c, text) in cols.iter().enumerate().skip(1) {
                let mut t = wide(text);
                let mut sub = LVITEMW { iSubItem: c as i32, pszText: PWSTR(t.as_mut_ptr()), ..Default::default() };
                send(list, LVM_SETITEMTEXTW, i, &mut sub as *mut _ as isize);
            }
        }
        send(list, WM_SETREDRAW, 1, 0);
        let _ = InvalidateRect(list, None, false);
    }
    IDS.with(|ids| *ids.borrow_mut() = rows.iter().map(|r| r.id).collect());
    if let Some(id) = selected_id {
        if let Some(i) = rows.iter().position(|r| r.id == id) {
            let mut item = LVITEMW { mask: LVIF_STATE, stateMask: LVIS_SELECTED, state: LVIS_SELECTED, ..Default::default() };
            send(list, LVM_SETITEMSTATE, i, &mut item as *mut _ as isize);
        }
    }
    let _ = win;
}

fn selected_id(h: HWND) -> Option<u32> {
    let list = child(h, ID_LIST);
    let i = send(list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
    (i >= 0).then(|| IDS.with(|ids| ids.borrow().get(i as usize).copied())).flatten()
}

unsafe fn layout(hwnd: HWND) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right, rc.bottom);
    let (m, bh, gap) = (px(8), px(26), px(6));
    let list_h = (h - bh - m * 3).max(0);
    let _ = MoveWindow(child(hwnd, ID_LIST), m, m, w - m * 2, list_h, true);
    let y = h - bh - m;
    let mut x = m;
    for (id, width) in [(ID_CANCEL, 110), (ID_OPEN, 90), (ID_FOLDER, 130), (ID_CLEAR, 120), (ID_LOG, 100)] {
        let _ = MoveWindow(child(hwnd, id), x, y, px(width), bh, true);
        x += px(width) + gap;
    }
    let _ = MoveWindow(child(hwnd, ID_CLOSE), w - m - px(90), y, px(90), bh, true);
}

unsafe fn create_controls(hwnd: HWND) {
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
    let list = make(
        WC_LISTVIEWW, "", WS_TABSTOP | WINDOW_STYLE(LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS), WS_EX_CLIENTEDGE, ID_LIST,
    );
    send(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize);
    for (i, (name, width, right)) in
        [("Peer", 170, false), ("File", 210, false), ("Size", 80, true), ("Done", 55, true), ("Speed", 85, true), ("Status", 260, false)]
            .iter()
            .enumerate()
    {
        let mut t = wide(name);
        let mut c = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH | LVCF_FMT | LVCF_SUBITEM,
            fmt: if *right { LVCFMT_RIGHT } else { LVCFMT_LEFT },
            cx: px(*width),
            pszText: PWSTR(t.as_mut_ptr()),
            iSubItem: i as i32,
            ..Default::default()
        };
        send(list, LVM_INSERTCOLUMNW, i, &mut c as *mut _ as isize);
    }
    if let Some(w) = app::saved_columns("dcc", 6) {
        crate::listcols::apply(list, &w);
    }
    let push = WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32);
    make(w!("BUTTON"), "Cancel Selected", push, WINDOW_EX_STYLE(0), ID_CANCEL);
    make(w!("BUTTON"), "Open File", push, WINDOW_EX_STYLE(0), ID_OPEN);
    make(w!("BUTTON"), "Show in Folder", push, WINDOW_EX_STYLE(0), ID_FOLDER);
    make(w!("BUTTON"), "Clear Finished", push, WINDOW_EX_STYLE(0), ID_CLEAR);
    make(w!("BUTTON"), "View Log", push, WINDOW_EX_STYLE(0), ID_LOG);
    make(w!("BUTTON"), "Close", push, WINDOW_EX_STYLE(0), ID_CLOSE);
}

extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                create_controls(hwnd);
                layout(hwnd);
                SetTimer(hwnd, TIMER, 500, None);
                LRESULT(0)
            }
            WM_SIZE => {
                layout(hwnd);
                LRESULT(0)
            }
            WM_GETMINMAXINFO => {
                let mm = &mut *(lp.0 as *mut MINMAXINFO);
                mm.ptMinTrackSize.x = px(560);
                mm.ptMinTrackSize.y = px(200);
                LRESULT(0)
            }
            WM_TIMER => {
                refresh(); // speeds change while transfers run
                LRESULT(0)
            }
            WM_COMMAND => {
                match wp.0 & 0xffff {
                    ID_CANCEL => {
                        if let Some(id) = selected_id(hwnd) {
                            app::dcc_cancel(id);
                        }
                    }
                    ID_OPEN => {
                        if let Some(id) = selected_id(hwnd) {
                            app::dcc_open_file(id);
                        }
                    }
                    ID_FOLDER => {
                        if let Some(id) = selected_id(hwnd) {
                            app::dcc_show(id);
                        }
                    }
                    ID_CLEAR => app::dcc_clear_finished(),
                    ID_LOG => app::dcc_open_log(),
                    ID_CLOSE => {
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_NOTIFY => {
                let nm = &*(lp.0 as *const NMHDR);
                if nm.idFrom == ID_LIST && nm.code == NM_DBLCLK {
                    if let Some(id) = selected_id(hwnd) {
                        app::dcc_show(id);
                    }
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                app::remember_columns("dcc", &crate::listcols::read(child(hwnd, ID_LIST), 6));
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_NCDESTROY => {
                let _ = KillTimer(hwnd, TIMER);
                WINDOW.with(|w| w.set(0));
                IDS.with(|ids| ids.borrow_mut().clear());
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
