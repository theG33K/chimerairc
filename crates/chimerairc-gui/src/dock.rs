//! The docked pane area at the left of the main window (HydraIRC's dockable panes, reduced to one
//! dock): a tab strip over the panes that are switched on, and a splitter on the right edge to
//! resize it.
//!
//! * **Servers**: a tree of networks and their servers (double click connects).
//! * **Events**: connections, disconnects, reconnects, DCC results, in one log.
//! * **Messages**: private messages received (double click opens the chat).
//! * **Notices**: NOTICEs received, from people and from servers.
//! * **URLs**: every web address seen in any window (double click opens it).
//! * **Channels**: a monitor of the channels you are in, with user counts and topics.
//! * **Connections**: a monitor of your connections and what state each is in.
//! * **Buddies**: the people you want to keep an eye on, and whether they are online.
//!
//! Which panes have a tab, which one is in front, whether the dock is shown and how wide it is are
//! saved, and five such layouts can be stored and recalled from the View menu.

use crate::app;
use crate::layouts::{self, Pane};
use crate::rowfilter;
use crate::servers::{self, Server};
use crate::winplace::{self, Placement};
use crate::ui;
use crate::util::*;
use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU16, AtomicU64, AtomicU8, Ordering};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, ReleaseCapture, SetCapture, VK_DELETE, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("ChimeraIRC.Dock");
const ID_TABS: usize = 1;
const ID_TREE: usize = 2;
const ID_EVENTS: usize = 3;
const ID_OUTPUT: usize = 4;
/// The filter box above each list pane has id `ID_FILTER_BASE` plus the pane's number; it travels with its pane.
const ID_FILTER_BASE: usize = 20;
/// While a tab is dragged this timer watches for the Escape key (key presses go to the focused window,
/// not to the window that has captured the mouse).
const TAB_TIMER: usize = 0x7a1;
/// The list panes have ids `ID_LIST_BASE + pane index`.
const ID_LIST_BASE: usize = 10;
/// Width of the splitter strip on the right edge, in 96-dpi pixels.
const GRIP: i32 = 5;
const MIN_WIDTH: i32 = layouts::MIN_WIDTH as i32;
const MAX_WIDTH: i32 = layouts::MAX_WIDTH as i32;
const MENU_CONNECT: usize = 1;
const MENU_ADD: usize = 2;
const MENU_EDIT: usize = 3;
const MENU_REMOVE: usize = 4;
const MENU_OPEN: usize = 5;
const MENU_COPY: usize = 6;
const MENU_CLEAR: usize = 7;
const MENU_GOTO: usize = 8;
const MENU_DISCONNECT: usize = 9;
const MENU_WHOIS: usize = 10;
/// Tab menu: `MENU_TO_EDGE + edge index` moves the pane there; `MENU_HIDE_TAB` switches the tab off.
const MENU_TO_EDGE: usize = 20;
const MENU_HIDE_TAB: usize = 24;
/// Tab menu: move the pane to the other half of its dock, or join the two halves.
const MENU_OTHER_HALF: usize = 25;
const MENU_MERGE: usize = 26;
/// Users pane: add to the buddy list / ignore. Buddies pane: fold groups.
const MENU_BUDDY_ADD: usize = 11;
const MENU_IGNORE: usize = 12;
const MENU_FOLD: usize = 13;
const MENU_FOLD_ALL: usize = 14;
const MENU_UNFOLD_ALL: usize = 15;
/// The event log keeps about this many characters; older lines scroll away.
const EVENT_LOG_LIMIT: usize = 60_000;
/// The list panes keep this many rows; older ones scroll away.
const ROW_LIMIT: usize = 500;

// One window per dock half (a slot, see `layouts::slot`); a half with no panes is hidden.
static DOCKS: [AtomicIsize; 8] = [const { AtomicIsize::new(0) }; 8];
static TABS: [AtomicIsize; 8] = [const { AtomicIsize::new(0) }; 8];
static VISIBLE: [AtomicBool; 4] = [const { AtomicBool::new(false) }; 4];
static SIZE96: [AtomicI32; 4] = [const { AtomicI32::new(220) }; 4];
static ACTIVE: [AtomicU8; 8] = [const { AtomicU8::new(0) }; 8];
static MASK: [AtomicU16; 8] = [const { AtomicU16::new(0) }; 8];
/// For a split dock, the share (percent) the first half takes, by edge.
static SPLIT: [AtomicI32; 4] = [const { AtomicI32::new(50) }; 4];
/// The slot whose splitter is being dragged, or -1, and whether it is the divider between two halves.
static DRAGGING: AtomicI32 = AtomicI32::new(-1);
static DRAG_DIVIDER: AtomicBool = AtomicBool::new(false);
/// The window of each floating pane (by pane index), 0 when it does not float, and whether it is shown.
static FLOATS: [AtomicIsize; layouts::PANE_COUNT] = [const { AtomicIsize::new(0) }; layouts::PANE_COUNT];
static FLOAT_SHOWN: [AtomicBool; layouts::PANE_COUNT] = [const { AtomicBool::new(false) }; layouts::PANE_COUNT];
/// The original window procedure of each dock's tab control (it is subclassed to drag tabs).
static TAB_ORIG: [AtomicIsize; 8] = [const { AtomicIsize::new(0) }; 8];
static FLOAT_REGISTERED: std::sync::Once = std::sync::Once::new();
/// The tab order of each dock: four bits per tab, the pane number plus one (0 = no more tabs).
static ORDER: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
/// The window that shows where a dragged tab would land.
static HINT: AtomicIsize = AtomicIsize::new(0);
static HINT_REGISTERED: std::sync::Once = std::sync::Once::new();
const HINT_CLASS: PCWSTR = w!("ChimeraIRC.DropHint");
const FLOAT_CLASS: PCWSTR = w!("ChimeraIRC.Float");

/// A tab being dragged: where the button went down and whether it has moved far enough to count.
#[derive(Clone, Copy)]
struct TabDrag {
    /// The dock half whose tab it is.
    slot: usize,
    /// The pane being dragged (found before the click changes the tabs).
    pane: Pane,
    x: i32,
    y: i32,
    active: bool,
}
/// The pane controls (tree, event log, list views) by control id. They are made once and move
/// between the dock windows as panes are moved.
static CTRL: [AtomicIsize; 32] = [const { AtomicIsize::new(0) }; 32];
static REGISTERED: std::sync::Once = std::sync::Once::new();
/// The saved filter texts are being put into the boxes: do not save them again.
static LOADING_FILTERS: AtomicBool = AtomicBool::new(false);

/// What a row of a list pane stands for, so double click and the context menu know what to do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Target {
    None,
    Query { conn: u32, nick: String },
    Url(String),
    Channel { conn: u32, name: String },
    Conn(u32),
    /// A buddy; `conn` is a connection they are online on, if any.
    Buddy { nick: String, conn: Option<u32> },
    /// The heading of a buddy group (its key); activating it folds or unfolds the group.
    BuddyGroup { key: String },
    /// A person in the global user list.
    User { conn: u32, nick: String },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Row {
    pub cells: Vec<String>,
    pub target: Target,
}

thread_local! {
    /// The servers behind the tree's leaves (a leaf's lParam is its index + 1).
    static ITEMS: RefCell<Vec<Server>> = const { RefCell::new(Vec::new()) };
    /// The rows of each list pane, indexed by pane.
    static ROWS: RefCell<[Vec<Row>; layouts::PANE_COUNT]> = const { RefCell::new([const { Vec::new() }; layouts::PANE_COUNT]) };
    /// The pane behind each tab of each dock, in tab order.
    static TAB_PANES: RefCell<[Vec<Pane>; 8]> = const { RefCell::new([const { Vec::new() }; 8]) };
    static TAB_DRAG: std::cell::Cell<Option<TabDrag>> = const { std::cell::Cell::new(None) };
    /// The words typed in each list pane's filter box, lower-cased.
    static TERMS: RefCell<[Vec<String>; layouts::PANE_COUNT]> = const { RefCell::new([const { Vec::new() }; layouts::PANE_COUNT]) };
    /// For a filtered list pane, which of its rows show (indices into `ROWS`); `None` when all do.
    static VIEW: RefCell<[Option<Vec<usize>>; layouts::PANE_COUNT]> = const { RefCell::new([const { None }; layouts::PANE_COUNT]) };
}

/// Columns of a list pane: (title, width in 96-dpi pixels).
fn columns(p: Pane) -> &'static [(&'static str, i32)] {
    match p {
        Pane::Messages => &[("Time", 60), ("From", 80), ("Message", 400)],
        Pane::Notices => &[("Time", 60), ("From", 80), ("To", 70), ("Notice", 400)],
        Pane::Urls => &[("Time", 60), ("Address", 300), ("Where", 100)],
        Pane::Channels => &[("Server", 100), ("Channel", 90), ("Users", 45), ("Topic", 300)],
        Pane::Connections => &[("Server", 120), ("Nick", 80), ("State", 90), ("Channels", 60)],
        Pane::Buddies => &[("Buddy", 100), ("Status", 60), ("Group", 70), ("Where / note", 160)],
        Pane::Users => &[("Nick", 110), ("Server", 110), ("Channels", 240)],
        Pane::Servers | Pane::Events | Pane::Output => &[],
    }
}

fn is_list_pane(p: Pane) -> bool {
    !columns(p).is_empty()
}

/// The window of the first half of the dock on `e` (null before it is created).
pub fn handle(e: layouts::Edge) -> HWND {
    handle_slot(e as usize)
}

fn handle_slot(s: usize) -> HWND {
    HWND(DOCKS.get(s).map(|d| d.load(Ordering::Relaxed)).unwrap_or(0) as *mut c_void)
}

/// Any dock window, as the owner of pop-up menus.
fn owner() -> HWND {
    (0..8).map(handle_slot).find(|h| !h.0.is_null()).unwrap_or_default()
}

/// Which slot a dock window is.
fn slot_of(hwnd: HWND) -> usize {
    (0..8).find(|s| handle_slot(*s) == hwnd).unwrap_or(0)
}

fn edge_of(hwnd: HWND) -> layouts::Edge {
    layouts::slot_edge(slot_of(hwnd))
}

fn tabs_of(s: usize) -> HWND {
    HWND(TABS.get(s).map(|t| t.load(Ordering::Relaxed)).unwrap_or(0) as *mut c_void)
}

/// A pane control by id (null before it is created).
fn child(id: usize) -> HWND {
    CTRL.get(id).map(|c| HWND(c.load(Ordering::Relaxed) as *mut c_void)).unwrap_or_default()
}

/// The id of the filter box of a list pane.
fn filter_id(p: Pane) -> usize {
    ID_FILTER_BASE + p as usize
}

/// A control that goes with a pane's list and moves, shows and hides with it: its filter box.
fn companion(p: Pane) -> Option<HWND> {
    is_list_pane(p).then(|| child(filter_id(p))).filter(|h| !h.0.is_null())
}

/// The settings key of a pane's filter text.
fn filter_key(p: Pane) -> String {
    p.title().to_ascii_lowercase()
}

/// A command from a control of a dock or floating window: the text of a filter box changed.
fn on_command(wp: WPARAM, lp: LPARAM) {
    if (wp.0 >> 16) as u32 != EN_CHANGE {
        return;
    }
    let src = HWND(lp.0 as *mut c_void);
    if let Some(p) = layouts::PANES.iter().copied().find(|p| is_list_pane(*p) && child(filter_id(*p)) == src) {
        filter_changed(p);
    }
}

/// The text of a pane's filter box changed: show only the matching rows, and remember the text.
fn filter_changed(p: Pane) {
    let edit = child(filter_id(p));
    if edit.0.is_null() {
        return;
    }
    let text = unsafe {
        let n = GetWindowTextLengthW(edit).max(0) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(edit, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..got])
    };
    TERMS.with(|t| t.borrow_mut()[p as usize] = rowfilter::terms(&text));
    rebuild_view(p);
    sync_count(p);
    if !LOADING_FILTERS.load(Ordering::Relaxed) {
        app::remember_filter(&filter_key(p), &text);
    }
}

/// Work out which rows of a pane show for its filter.
fn rebuild_view(p: Pane) {
    let terms = TERMS.with(|t| t.borrow()[p as usize].clone());
    let view = (!terms.is_empty()).then(|| {
        ROWS.with(|r| {
            let rows = &r.borrow()[p as usize];
            let cells: Vec<&[String]> = rows.iter().map(|x| x.cells.as_slice()).collect();
            let headers: Vec<bool> = rows.iter().map(|x| matches!(x.target, Target::BuddyGroup { .. })).collect();
            rowfilter::visible(&cells, &headers, &terms)
        })
    });
    VIEW.with(|v| v.borrow_mut()[p as usize] = view);
}

/// How many rows of a pane show.
fn shown_len(p: Pane) -> usize {
    VIEW.with(|v| match &v.borrow()[p as usize] {
        Some(ix) => ix.len(),
        None => ROWS.with(|r| r.borrow()[p as usize].len()),
    })
}

/// Put pane `p` into the rectangle (x, y, w, h) of its window: the Users list leaves room for its filter box.
unsafe fn place_pane(p: Pane, x: i32, y: i32, w: i32, h: i32) {
    if let Some(f) = companion(p) {
        let fh = px(24).min(h);
        let _ = MoveWindow(f, x, y, w, fh, true);
        let _ = MoveWindow(control_of(p), x, y + fh, w, (h - fh).max(0), true);
    } else {
        let _ = MoveWindow(control_of(p), x, y, w, h, true);
    }
}

/// Make `ctl` a child of `parent` (when it is not already).
unsafe fn adopt(ctl: HWND, parent: HWND) {
    if !ctl.0.is_null() && !parent.0.is_null() && GetParent(ctl).ok() != Some(parent) {
        let _ = SetParent(ctl, parent);
    }
}

/// The control that shows a pane.
fn control_of(p: Pane) -> HWND {
    match p {
        Pane::Servers => child(ID_TREE),
        Pane::Events => child(ID_EVENTS),
        Pane::Output => child(ID_OUTPUT),
        _ => list_of(p),
    }
}

fn list_of(p: Pane) -> HWND {
    child(ID_LIST_BASE + p as usize)
}

fn send(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    unsafe { SendMessageW(h, msg, WPARAM(wp), LPARAM(lp)).0 }
}

/// Thickness of the dock on `e` in device pixels (its width, or its height on the top or bottom): 0
/// while it is hidden or there is none.
pub fn size_px(e: layouts::Edge) -> i32 {
    if VISIBLE[e as usize].load(Ordering::Relaxed) && MASK[e as usize].load(Ordering::Relaxed) != 0 {
        px(SIZE96[e as usize].load(Ordering::Relaxed))
    } else {
        0
    }
}

pub fn create(parent: HWND) {
    REGISTERED.call_once(|| unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: ui::hinstance(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as usize as *mut c_void),
            ..Default::default()
        };
        RegisterClassW(&wc);
    });
    unsafe {
        // the left dock goes first: it creates the pane controls, the others only their tab strips
        for s in 0..8usize {
            if let Ok(h) = CreateWindowExW(
                WINDOW_EX_STYLE(0), CLASS, PCWSTR::null(), WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0, 0, 0, 0, parent, None, ui::hinstance(), Some(s as *const c_void),
            ) {
                crate::look::style_tree(h);
            }
        }
    }
}

/// Apply a layout: for every dock whether it is shown, its thickness (96-dpi pixels), the panes that
/// have a tab and the one in front; panes are moved to the dock that holds them.
/// Does not lay out the main window; `ui::apply_dock` does that afterwards.
pub fn apply(l: layouts::Layout) {
    let l = l.normalized();
    for e in layouts::EDGES {
        let d = l.dock(e);
        VISIBLE[e as usize].store(d.visible, Ordering::Relaxed);
        SIZE96[e as usize].store(d.size as i32, Ordering::Relaxed);
        SPLIT[e as usize].store(l.split[e as usize] as i32, Ordering::Relaxed);
        for g in 0..2 {
            let s = layouts::slot(e, g);
            let h = l.half(e, g);
            ACTIVE[s].store(h.active, Ordering::Relaxed);
            MASK[s].store(h.panes, Ordering::Relaxed);
            ORDER[s].store(pack_order(&l.ordered_in(e, g)), Ordering::Relaxed);
        }
    }
    if owner().0.is_null() {
        return;
    }
    // every pane control lives in the dock half that has its pane (or stays where it is while it has none)
    for p in layouts::PANES {
        let (ctl, Some((e, g))) = (control_of(p), l.half_of(p)) else { continue };
        let dock = handle_slot(layouts::slot(e, g));
        unsafe {
            adopt(ctl, dock);
            if let Some(c) = companion(p) {
                adopt(c, dock);
            }
        }
    }
    // floating panes get a window of their own; those that no longer float lose theirs
    let screens = ui::screens();
    for p in layouts::PANES {
        let i = p as usize;
        match l.floats[i] {
            Some(f) => {
                let r = winplace::fit_min(
                    Placement { left: f.left, top: f.top, width: f.width, height: f.height, maximized: false },
                    &screens,
                    layouts::FLOAT_MIN_W,
                    layouts::FLOAT_MIN_H,
                );
                let h = ensure_float(p);
                let ctl = control_of(p);
                unsafe {
                    let _ = SetWindowPos(h, None, r.left, r.top, r.width, r.height, SWP_NOZORDER | SWP_NOACTIVATE);
                    adopt(ctl, h);
                    if let Some(c) = companion(p) {
                        adopt(c, h);
                    }
                    FLOAT_SHOWN[i].store(f.visible, Ordering::Relaxed);
                    let _ = ShowWindow(h, if f.visible { SW_SHOWNA } else { SW_HIDE });
                    float_layout(h);
                }
            }
            None => {
                let h = HWND(FLOATS[i].swap(0, Ordering::Relaxed) as *mut c_void);
                if !h.0.is_null() {
                    unsafe {
                        // the control must outlive the window: park it in a dock before the window goes
                        let ctl = control_of(p);
                        if !ctl.0.is_null() && GetParent(ctl).ok() == Some(h) {
                            let _ = SetParent(ctl, owner());
                            if let Some(c) = companion(p) {
                                let _ = SetParent(c, owner());
                            }
                        }
                        FLOAT_SHOWN[i].store(false, Ordering::Relaxed);
                        let _ = DestroyWindow(h);
                    }
                }
            }
        }
    }
    for s in 0..8usize {
        let (e, g) = (layouts::slot_edge(s), layouts::slot_half(s));
        let h = handle_slot(s);
        if !h.0.is_null() {
            let shown = l.dock(e).shown() && l.half(e, g).panes != 0;
            unsafe {
                let _ = ShowWindow(h, if shown { SW_SHOWNA } else { SW_HIDE });
            }
        }
        rebuild_tabs(s);
    }
    show_pane_controls();
}

fn pack_order(panes: &[Pane]) -> u64 {
    panes.iter().take(layouts::PANE_COUNT).enumerate().fold(0, |acc, (i, p)| acc | ((*p as u64 + 1) << (4 * i)))
}

fn unpack_order(v: u64) -> Vec<Pane> {
    (0..layouts::PANE_COUNT).map_while(|i| match (v >> (4 * i)) & 0xf {
        0 => None,
        n => Pane::from_index(n as u8 - 1),
    }).collect()
}

pub fn current() -> layouts::Layout {
    let mut l = layouts::Layout::default();
    for e in layouts::EDGES {
        let i = e as usize;
        l.docks[i] = layouts::Dock {
            visible: VISIBLE[i].load(Ordering::Relaxed),
            size: SIZE96[i].load(Ordering::Relaxed).max(0) as u32,
            panes: MASK[i].load(Ordering::Relaxed),
            active: ACTIVE[i].load(Ordering::Relaxed),
        };
        let s2 = layouts::slot(e, 1);
        l.second[i] = layouts::Dock {
            visible: false,
            size: e.default_size(),
            panes: MASK[s2].load(Ordering::Relaxed),
            active: ACTIVE[s2].load(Ordering::Relaxed),
        };
        l.split[i] = SPLIT[i].load(Ordering::Relaxed).clamp(0, 100) as u8;
    }
    for s in 0..8usize {
        l.set_order_in(layouts::slot_edge(s), layouts::slot_half(s), &unpack_order(ORDER[s].load(Ordering::Relaxed)));
    }
    for p in layouts::PANES {
        let h = HWND(FLOATS[p as usize].load(Ordering::Relaxed) as *mut c_void);
        if h.0.is_null() {
            continue;
        }
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(h, &mut r);
        }
        l.floats[p as usize] = Some(layouts::Float {
            left: r.left,
            top: r.top,
            width: r.right - r.left,
            height: r.bottom - r.top,
            visible: FLOAT_SHOWN[p as usize].load(Ordering::Relaxed),
        });
    }
    l.normalized()
}

/// The window of a floating pane, made when it is needed.
fn ensure_float(p: Pane) -> HWND {
    let i = p as usize;
    let existing = HWND(FLOATS[i].load(Ordering::Relaxed) as *mut c_void);
    if !existing.0.is_null() {
        return existing;
    }
    FLOAT_REGISTERED.call_once(|| unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(float_proc),
            hInstance: ui::hinstance(),
            lpszClassName: FLOAT_CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as usize as *mut c_void),
            ..Default::default()
        };
        RegisterClassW(&wc);
    });
    let title = wide(p.title());
    let h = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            FLOAT_CLASS,
            PCWSTR(title.as_ptr()),
            WS_POPUP | WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_CLIPCHILDREN,
            0, 0, px(360), px(300),
            ui::frame(), None, ui::hinstance(), Some(i as *const c_void),
        )
        .unwrap_or_default()
    };
    FLOATS[i].store(h.0 as isize, Ordering::Relaxed);
    crate::look::style_tree(h);
    h
}

/// Make the pane control fill the client area of its floating window.
unsafe fn float_layout(hwnd: HWND) {
    let Some(p) = float_pane_of(hwnd) else { return };
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    place_pane(p, 0, 0, rc.right, rc.bottom);
}

fn float_pane_of(hwnd: HWND) -> Option<Pane> {
    layouts::PANES.iter().copied().find(|p| FLOATS[*p as usize].load(Ordering::Relaxed) == hwnd.0 as isize)
}

/// One tab per pane of the dock half in slot `s`, the active one selected.
fn rebuild_tabs(s: usize) {
    let tabs = tabs_of(s);
    if tabs.0.is_null() {
        return;
    }
    let panes = unpack_order(ORDER[s].load(Ordering::Relaxed));
    send(tabs, TCM_DELETEALLITEMS, 0, 0);
    for (n, p) in panes.iter().enumerate() {
        let mut t = wide(p.title());
        let item = TCITEMW { mask: TCIF_TEXT, pszText: PWSTR(t.as_mut_ptr()), ..Default::default() };
        send(tabs, TCM_INSERTITEMW, n, &item as *const _ as isize);
    }
    // owner-drawn tabs all have one size: wide enough for the longest name
    unsafe {
        let dc = GetDC(tabs);
        let old = SelectObject(dc, HGDIOBJ(ui::ui_font_raw() as *mut c_void));
        let mut widest = 0;
        for p in &panes {
            let t: Vec<u16> = p.title().encode_utf16().collect();
            let mut size = windows::Win32::Foundation::SIZE::default();
            let _ = GetTextExtentPoint32W(dc, &t, &mut size);
            widest = widest.max(size.cx);
        }
        SelectObject(dc, old);
        ReleaseDC(tabs, dc);
        send(tabs, TCM_SETITEMSIZE, 0, ((px(22) as isize) << 16) | ((widest + px(18)) as isize & 0xffff));
    }
    let active = Pane::from_index(ACTIVE[s].load(Ordering::Relaxed)).unwrap_or(Pane::Servers);
    let sel = panes.iter().position(|p| *p == active).unwrap_or(0);
    TAB_PANES.with(|t| t.borrow_mut()[s] = panes);
    send(tabs, TCM_SETCURSEL, sel, 0);
    unsafe {
        layout(handle_slot(s)); // the tab strip may now need more or fewer rows
    }
}

/// Show the pane in front of every dock half and hide all other pane controls.
fn show_pane_controls() {
    unsafe {
        for p in layouts::PANES {
            let ctl = control_of(p);
            let front = FLOAT_SHOWN[p as usize].load(Ordering::Relaxed)
                || (0..8).any(|s| MASK[s].load(Ordering::Relaxed) & p.bit() != 0 && ACTIVE[s].load(Ordering::Relaxed) == p as u8);
            let _ = ShowWindow(ctl, if front { SW_SHOW } else { SW_HIDE });
            if let Some(c) = companion(p) {
                let _ = ShowWindow(c, if front { SW_SHOW } else { SW_HIDE });
            }
        }
    }
}

/// Put the dock on `e` into the rectangle `r` of the main window: its one window, or the two halves side
/// by side (on the top and bottom) or one above the other (left and right).
pub fn place_dock(e: layouts::Edge, r: layouts::Rect4) {
    let (x, y, w, h) = r;
    let (first, second) = (handle_slot(layouts::slot(e, 0)), handle_slot(layouts::slot(e, 1)));
    unsafe {
        let split = MASK[layouts::slot(e, 1)].load(Ordering::Relaxed) != 0 && w > 0 && h > 0;
        if !split {
            let _ = MoveWindow(first, x, y, w, h, true);
            let _ = MoveWindow(second, x, y, 0, 0, false);
            return;
        }
        let pct = SPLIT[e as usize].load(Ordering::Relaxed).clamp(layouts::SPLIT_MIN as i32, layouts::SPLIT_MAX as i32);
        if e.horizontal() {
            let w1 = w * pct / 100;
            let _ = MoveWindow(first, x, y, w1, h, true);
            let _ = MoveWindow(second, x + w1, y, w - w1, h, true);
        } else {
            let h1 = h * pct / 100;
            let _ = MoveWindow(first, x, y, w, h1, true);
            let _ = MoveWindow(second, x, y + h1, w, h - h1, true);
        }
    }
}

// ---------------------------------------------------------------- the server list tree

fn tree() -> HWND {
    child(ID_TREE)
}

fn tv_text(item: isize) -> String {
    let mut buf = [0u16; 128];
    let mut it = TVITEMW { mask: TVIF_TEXT | TVIF_HANDLE, hItem: HTREEITEM(item), pszText: PWSTR(buf.as_mut_ptr()), cchTextMax: 128, ..Default::default() };
    send(tree(), TVM_GETITEMW, 0, &mut it as *mut _ as isize);
    let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// Networks that are currently expanded, so a rebuild can put them back.
fn expanded_networks() -> HashSet<String> {
    let mut out = HashSet::new();
    let mut item = send(tree(), TVM_GETNEXTITEM, TVGN_ROOT as usize, 0);
    while item != 0 {
        let mut it = TVITEMW { mask: TVIF_STATE | TVIF_HANDLE, hItem: HTREEITEM(item), stateMask: TVIS_EXPANDED, ..Default::default() };
        send(tree(), TVM_GETITEMW, 0, &mut it as *mut _ as isize);
        if it.state.0 & TVIS_EXPANDED.0 != 0 {
            out.insert(tv_text(item));
        }
        item = send(tree(), TVM_GETNEXTITEM, TVGN_NEXT as usize, item);
    }
    out
}

fn insert(parent: isize, text: &str, param: isize) -> isize {
    let mut t = wide(text);
    let ins = TVINSERTSTRUCTW {
        hParent: HTREEITEM(parent),
        hInsertAfter: HTREEITEM(-2), // TVI_LAST
        Anonymous: TVINSERTSTRUCTW_0 {
            item: TVITEMW { mask: TVIF_TEXT | TVIF_PARAM, pszText: PWSTR(t.as_mut_ptr()), lParam: LPARAM(param), ..Default::default() },
        },
    };
    send(tree(), TVM_INSERTITEMW, 0, &ins as *const _ as isize)
}

/// Rebuild the tree from the (already merged and sorted) server list.
pub fn refresh_servers(list: &[Server]) {
    if tree().0.is_null() {
        return;
    }
    let open = expanded_networks();
    let selected = selected_server();
    unsafe {
        let _ = SendMessageW(tree(), WM_SETREDRAW, WPARAM(0), LPARAM(0));
    }
    send(tree(), TVM_DELETEITEM, 0, 0); // TVI_ROOT: delete everything
    let flat: Vec<Server> = list.to_vec();
    ITEMS.with(|i| *i.borrow_mut() = flat.clone());
    let mut index = 0usize;
    let mut reselect = 0isize;
    for (network, servers) in servers::groups(&flat) {
        let root = insert(0, &network, 0);
        for s in &servers {
            index += 1;
            let item = insert(root, &s.label(), index as isize);
            if selected.as_ref().is_some_and(|sel| sel.same_endpoint(&s.host, s.port)) {
                reselect = item;
            }
        }
        if open.contains(&network) {
            send(tree(), TVM_EXPAND, TVE_EXPAND.0 as usize, root);
        }
    }
    if reselect != 0 {
        send(tree(), TVM_SELECTITEM, TVGN_CARET as usize, reselect);
    }
    unsafe {
        let _ = SendMessageW(tree(), WM_SETREDRAW, WPARAM(1), LPARAM(0));
        let _ = InvalidateRect(tree(), None, true);
    }
}

/// What a tree item stands for: a network (its name) or a server.
enum TreeTarget {
    Network(String),
    Server(Server),
}

fn target_of(item: isize) -> Option<TreeTarget> {
    if item == 0 {
        return None;
    }
    let mut it = TVITEMW { mask: TVIF_PARAM | TVIF_HANDLE, hItem: HTREEITEM(item), ..Default::default() };
    send(tree(), TVM_GETITEMW, 0, &mut it as *mut _ as isize);
    let param = it.lParam.0;
    if param == 0 {
        Some(TreeTarget::Network(tv_text(item)))
    } else {
        ITEMS.with(|i| i.borrow().get((param - 1) as usize).cloned()).map(TreeTarget::Server)
    }
}

fn selected_item() -> isize {
    send(tree(), TVM_GETNEXTITEM, TVGN_CARET as usize, 0)
}

fn selected_server() -> Option<Server> {
    match target_of(selected_item()) {
        Some(TreeTarget::Server(s)) => Some(s),
        _ => None,
    }
}

fn activate_selected() {
    let item = selected_item();
    match target_of(item) {
        Some(TreeTarget::Server(s)) => app::server_list_connect(&s),
        Some(TreeTarget::Network(_)) => {
            send(tree(), TVM_EXPAND, TVE_TOGGLE.0 as usize, item);
        }
        None => {}
    }
}

fn tree_context_menu() {
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mut local = pt;
        let _ = ScreenToClient(tree(), &mut local);
        let mut hit = TVHITTESTINFO { pt: local, ..Default::default() };
        let item = send(tree(), TVM_HITTEST, 0, &mut hit as *mut _ as isize);
        if item != 0 {
            send(tree(), TVM_SELECTITEM, TVGN_CARET as usize, item);
        }
        let target = target_of(item);
        let menu = CreatePopupMenu().unwrap_or_default();
        let user_owned = matches!(&target, Some(TreeTarget::Server(s)) if app::server_is_user(&s.host, s.port));
        let add = |id: usize, text: PCWSTR, on: bool| {
            let _ = AppendMenuW(menu, if on { MF_STRING } else { MF_STRING | MF_GRAYED }, id, text);
        };
        add(MENU_CONNECT, w!("&Connect"), matches!(target, Some(TreeTarget::Server(_))));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(MENU_ADD, w!("&Add Server..."), true);
        add(MENU_EDIT, w!("&Edit..."), matches!(target, Some(TreeTarget::Server(_))));
        // removing the user's copy of a built-in server puts the built-in entry back
        let restores = matches!(&target, Some(TreeTarget::Server(s)) if servers::is_builtin(&s.host, s.port));
        add(MENU_REMOVE, if restores { w!("&Restore Default") } else { w!("&Remove") }, user_owned);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, owner(), None).0 as usize;
        let _ = DestroyMenu(menu);
        match (cmd, target) {
            (MENU_CONNECT, Some(TreeTarget::Server(s))) => app::server_list_connect(&s),
            (MENU_ADD, Some(TreeTarget::Network(n))) => app::server_list_add(Some(&n)),
            (MENU_ADD, Some(TreeTarget::Server(s))) => app::server_list_add(Some(&s.network)),
            (MENU_ADD, None) => app::server_list_add(None),
            (MENU_EDIT, Some(TreeTarget::Server(s))) => app::server_list_edit(&s),
            (MENU_REMOVE, Some(TreeTarget::Server(s))) => app::server_list_remove(&s),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- the event log

/// Append a line to the Event Log.
pub fn event(line: &str) {
    append_line(child(ID_EVENTS), line);
}

/// Append a line to the Output pane (everything printed in every window).
pub fn output(line: &str) {
    append_line(child(ID_OUTPUT), line);
}

/// Add a line at the end of a read-only text pane, forgetting the oldest text beyond the limit.
fn append_line(edit: HWND, line: &str) {
    if edit.0.is_null() {
        return;
    }
    unsafe {
        let len = GetWindowTextLengthW(edit) as usize;
        if len > EVENT_LOG_LIMIT {
            send(edit, EM_SETSEL, 0, (len - EVENT_LOG_LIMIT / 2) as isize);
            let empty = wide("");
            send(edit, EM_REPLACESEL, 0, empty.as_ptr() as isize);
        }
        let end = GetWindowTextLengthW(edit) as isize;
        send(edit, EM_SETSEL, end as usize, end);
        let text = wide(&format!("{line}\r\n"));
        send(edit, EM_REPLACESEL, 0, text.as_ptr() as isize);
        send(edit, EM_SCROLLCARET, 0, 0);
    }
}

/// Is `p` in front, in a dock that is shown or in a floating window? (Panes that are costly to fill are
/// only filled while someone can see them.)
pub fn is_front(p: Pane) -> bool {
    FLOAT_SHOWN[p as usize].load(Ordering::Relaxed)
        || (0..8).any(|s| {
            MASK[s].load(Ordering::Relaxed) & p.bit() != 0
                && ACTIVE[s].load(Ordering::Relaxed) == p as u8
                && VISIBLE[layouts::slot_edge(s) as usize].load(Ordering::Relaxed)
        })
}

// ---------------------------------------------------------------- the list panes

fn sync_count(p: Pane) {
    let list = list_of(p);
    if list.0.is_null() {
        return;
    }
    let n = shown_len(p);
    send(list, LVM_SETITEMCOUNT, n, LVSICF_NOSCROLL as isize);
    unsafe {
        let _ = InvalidateRect(list, None, false);
    }
}

/// Add a row at the bottom of a log-style pane (Messages, Notices, URLs) and scroll to it. A row for
/// an address already in the URL list moves it to the bottom instead of repeating it.
pub fn push_row(p: Pane, row: Row) {
    if !is_list_pane(p) {
        return;
    }
    ROWS.with(|r| {
        let mut r = r.borrow_mut();
        let rows = &mut r[p as usize];
        if p == Pane::Urls {
            rows.retain(|x| x.target != row.target);
        }
        rows.push(row);
        if rows.len() > ROW_LIMIT {
            rows.remove(0);
        }
    });
    rebuild_view(p);
    sync_count(p);
    let n = shown_len(p);
    if n > 0 {
        send(list_of(p), LVM_ENSUREVISIBLE, n - 1, 0);
    }
}

/// Replace the rows of a monitor pane (Channels, Connections). Nothing changes, and nothing
/// flickers, when the rows are the same as before.
pub fn set_rows(p: Pane, rows: Vec<Row>) {
    if !is_list_pane(p) {
        return;
    }
    let changed = ROWS.with(|r| {
        let mut r = r.borrow_mut();
        if r[p as usize] == rows {
            false
        } else {
            r[p as usize] = rows;
            true
        }
    });
    if changed {
        rebuild_view(p);
        sync_count(p);
    }
}

pub fn clear(p: Pane) {
    ROWS.with(|r| r.borrow_mut()[p as usize].clear());
    rebuild_view(p);
    sync_count(p);
}

fn row_at(p: Pane, i: i32) -> Option<Row> {
    if i < 0 {
        return None;
    }
    // the i-th row that shows is not necessarily the i-th row while a filter is on
    let index = VIEW.with(|v| match &v.borrow()[p as usize] {
        Some(ix) => ix.get(i as usize).copied(),
        None => Some(i as usize),
    })?;
    ROWS.with(|r| r.borrow()[p as usize].get(index).cloned())
}

fn selected_row(p: Pane) -> Option<Row> {
    let i = send(list_of(p), LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
    row_at(p, i as i32)
}

fn activate_row(p: Pane) {
    let Some(row) = selected_row(p) else { return };
    match row.target {
        Target::Query { conn, nick } => app::dock_open_query(conn, &nick),
        Target::Url(u) => app::dock_open_url(&u),
        Target::Channel { conn, name } => app::dock_goto_channel(conn, &name),
        Target::Conn(c) => app::dock_goto_connection(c),
        Target::Buddy { nick, conn: Some(c) } => app::dock_open_query(c, &nick),
        Target::User { conn, nick } => app::dock_open_query(conn, &nick),
        Target::BuddyGroup { key } => app::buddy_group_toggle(&key),
        Target::Buddy { conn: None, .. } | Target::None => {}
    }
}

fn list_context_menu(p: Pane) {
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mut local = pt;
        let list = list_of(p);
        let _ = ScreenToClient(list, &mut local);
        let mut hit = LVHITTESTINFO { pt: local, ..Default::default() };
        let i = send(list, LVM_HITTEST, 0, &mut hit as *mut _ as isize);
        if i >= 0 {
            let mut st = LVITEMW {
                stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
                state: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
                ..Default::default()
            };
            send(list, LVM_SETITEMSTATE, i as usize, &mut st as *mut _ as isize);
        }
        let row = row_at(p, i as i32);
        let menu = CreatePopupMenu().unwrap_or_default();
        let add = |id: usize, text: PCWSTR, on: bool| {
            let _ = AppendMenuW(menu, if on { MF_STRING } else { MF_STRING | MF_GRAYED }, id, text);
        };
        let has = row.is_some();
        match p {
            Pane::Messages => add(MENU_OPEN, w!("&Open Chat"), has),
            Pane::Urls => {
                add(MENU_OPEN, w!("&Open in Browser"), has);
                add(MENU_COPY, w!("&Copy Address"), has);
            }
            Pane::Channels => add(MENU_GOTO, w!("&Go to Channel"), has),
            Pane::Connections => {
                add(MENU_GOTO, w!("&Go to Server Window"), has);
                add(MENU_DISCONNECT, w!("&Disconnect"), has);
            }
            Pane::Users => {
                add(MENU_OPEN, w!("&Open Chat"), has);
                add(MENU_WHOIS, w!("&Whois"), has);
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                add(MENU_BUDDY_ADD, w!("Add to &Buddies"), has);
                add(MENU_IGNORE, w!("&Ignore"), has);
                add(MENU_COPY, w!("&Copy Nick"), has);
            }
            Pane::Buddies => {
                let on_header = matches!(row, Some(Row { target: Target::BuddyGroup { .. }, .. }));
                if on_header {
                    add(MENU_FOLD, w!("&Fold or Unfold Group"), true);
                }
                add(MENU_FOLD_ALL, w!("Fold &All Groups"), true);
                add(MENU_UNFOLD_ALL, w!("&Unfold All Groups"), true);
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                let online = matches!(row, Some(Row { target: Target::Buddy { conn: Some(_), .. }, .. }));
                add(MENU_OPEN, w!("&Open Chat"), online);
                add(MENU_WHOIS, w!("&Whois"), online);
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                add(MENU_ADD, w!("&Add Buddy..."), true);
                add(MENU_REMOVE, w!("&Remove Buddy"), has);
            }
            _ => {}
        }
        if matches!(p, Pane::Messages | Pane::Notices | Pane::Urls) {
            if has || p == Pane::Notices {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            }
            add(MENU_CLEAR, w!("C&lear List"), true);
        }
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, owner(), None).0 as usize;
        let _ = DestroyMenu(menu);
        match cmd {
            MENU_OPEN | MENU_GOTO => activate_row(p),
            MENU_COPY => match row {
                Some(Row { target: Target::Url(u), .. }) => ui::copy_to_clipboard(&u),
                Some(Row { target: Target::User { nick, .. }, .. }) => ui::copy_to_clipboard(&nick),
                _ => {}
            },
            MENU_BUDDY_ADD => {
                if let Some(Row { target: Target::User { conn, nick }, .. }) = row {
                    app::dock_add_buddy(conn, &nick);
                }
            }
            MENU_IGNORE => {
                if let Some(Row { target: Target::User { conn, nick }, .. }) = row {
                    app::dock_ignore(conn, &nick);
                }
            }
            MENU_FOLD => {
                if let Some(Row { target: Target::BuddyGroup { key }, .. }) = row {
                    app::buddy_group_toggle(&key);
                }
            }
            MENU_FOLD_ALL => app::buddy_groups_fold_all(true),
            MENU_UNFOLD_ALL => app::buddy_groups_fold_all(false),
            MENU_CLEAR => clear(p),
            MENU_ADD if p == Pane::Buddies => app::buddy_add_dialog(),
            MENU_REMOVE if p == Pane::Buddies => {
                if let Some(Row { target: Target::Buddy { nick, .. }, .. }) = row {
                    app::buddy_remove_from_dock(&nick);
                }
            }
            MENU_WHOIS => match row {
                Some(Row { target: Target::Buddy { nick, conn: Some(c) }, .. }) => app::dock_whois(c, &nick),
                Some(Row { target: Target::User { conn, nick }, .. }) => app::dock_whois(conn, &nick),
                _ => {}
            },
            MENU_DISCONNECT => {
                if let Some(Row { target: Target::Conn(c), .. }) = row {
                    app::disconnect(c);
                }
            }
            _ => {}
        }
    }
}

/// Draw one tab of a dock in the theme's colours.
unsafe fn draw_dock_tab(slot: usize, dis: &DRAWITEMSTRUCT) {
    let t = crate::look::theme();
    let selected = dis.itemState.0 & ODS_SELECTED.0 != 0;
    let (bg, fg) = if selected { (t.accent, t.accent_text) } else { (t.panel, t.panel_text) };
    let brush = CreateSolidBrush(crate::look::colorref(bg));
    FillRect(dis.hDC, &dis.rcItem, brush);
    let _ = DeleteObject(HGDIOBJ(brush.0));
    let edge = CreateSolidBrush(crate::look::colorref(t.border));
    FrameRect(dis.hDC, &dis.rcItem, edge);
    let _ = DeleteObject(HGDIOBJ(edge.0));
    let title = TAB_PANES.with(|p| p.borrow()[slot].get(dis.itemID as usize).map(|p| p.title())).unwrap_or("");
    let old = SelectObject(dis.hDC, HGDIOBJ(ui::ui_font_raw() as *mut c_void));
    SetBkMode(dis.hDC, TRANSPARENT);
    SetTextColor(dis.hDC, crate::look::colorref(fg));
    let mut text: Vec<u16> = title.encode_utf16().collect();
    let mut rc = dis.rcItem;
    if !text.is_empty() && rc.right > rc.left && rc.bottom > rc.top {
        text.push(0);
        let n = text.len() - 1;
        DrawTextW(dis.hDC, &mut text[..n], &mut rc, DT_SINGLELINE | DT_VCENTER | DT_CENTER | DT_NOPREFIX);
    }
    SelectObject(dis.hDC, old);
}

/// Colours for the text boxes and lists inside a dock or a floating window.
unsafe fn control_colors(wp: WPARAM) -> LRESULT {
    let (dc, t) = (HDC(wp.0 as *mut c_void), crate::look::theme());
    SetBkColor(dc, crate::look::colorref(t.bg));
    SetTextColor(dc, crate::look::colorref(t.fg));
    LRESULT(crate::look::bg_brush().0 as isize)
}

/// Right click on a tab: move its pane to another edge or to the other half of its dock, or switch the
/// tab off.
fn tab_context_menu(s: usize) {
    unsafe {
        let (e, g) = (layouts::slot_edge(s), layouts::slot_half(s));
        let tabs = tabs_of(s);
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mut local = pt;
        let _ = ScreenToClient(tabs, &mut local);
        let mut hit = TCHITTESTINFO { pt: local, ..Default::default() };
        let i = send(tabs, TCM_HITTEST, 0, &mut hit as *mut _ as isize);
        let Some(pane) = (i >= 0).then(|| TAB_PANES.with(|t| t.borrow()[s].get(i as usize).copied())).flatten() else {
            return;
        };
        let menu = CreatePopupMenu().unwrap_or_default();
        for to in layouts::EDGES {
            let t = wide(&format!("Move to {}", to.title()));
            let flags = if to == e { MF_STRING | MF_GRAYED } else { MF_STRING };
            let _ = AppendMenuW(menu, flags, MENU_TO_EDGE + to as usize, PCWSTR(t.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let is_split = MASK[layouts::slot(e, 1)].load(Ordering::Relaxed) != 0;
        if g == 0 {
            // a half needs at least one tab of its own to stay behind
            let can = MASK[s].load(Ordering::Relaxed).count_ones() >= 2;
            let (label, flags) = if is_split {
                (w!("Move to the &Other Half"), MF_STRING | if can { MF_ENABLED } else { MF_GRAYED })
            } else {
                (w!("&Split Dock: Move This Tab to a New Half"), MF_STRING | if can { MF_ENABLED } else { MF_GRAYED })
            };
            let _ = AppendMenuW(menu, flags, MENU_OTHER_HALF, label);
        } else {
            let _ = AppendMenuW(menu, MF_STRING, MENU_OTHER_HALF, w!("Move to the &First Half"));
        }
        if is_split {
            let _ = AppendMenuW(menu, MF_STRING, MENU_MERGE, w!("&Merge Halves"));
        }
        let _ = AppendMenuW(menu, MF_STRING, MENU_HIDE_TAB, w!("&Hide Tab"));
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, owner(), None).0 as usize;
        let _ = DestroyMenu(menu);
        match cmd {
            MENU_HIDE_TAB => app::dock_pane_switch(pane as usize),
            MENU_OTHER_HALF => app::dock_other_half(pane as usize),
            MENU_MERGE => app::dock_merge(e as usize),
            c if (MENU_TO_EDGE..MENU_TO_EDGE + 4).contains(&c) => app::dock_move_pane(pane as usize, c - MENU_TO_EDGE),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- the window

unsafe fn create_controls(hwnd: HWND, slot: usize) {
    let hi = ui::hinstance();
    let font = ui::ui_font_raw() as usize;
    let make = |class: PCWSTR, style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, id: usize| {
        let c = CreateWindowExW(ex, class, PCWSTR::null(), WS_CHILD | style, 0, 0, 0, 0, hwnd, HMENU(id as *mut c_void), hi, None)
            .unwrap_or_default();
        SendMessageW(c, WM_SETFONT, WPARAM(font), LPARAM(1));
        if id < CTRL.len() && id != ID_TABS {
            CTRL[id].store(c.0 as isize, Ordering::Relaxed);
        }
        c
    };
    let tabs = make(w!("SysTabControl32"), WS_VISIBLE | WINDOW_STYLE(TCS_FOCUSNEVER | TCS_MULTILINE | TCS_OWNERDRAWFIXED), WINDOW_EX_STYLE(0), ID_TABS);
    TABS[slot].store(tabs.0 as isize, Ordering::Relaxed);
    // subclass the tab strip so that a tab can be dragged to another dock or out of the window
    let orig = SetWindowLongPtrW(tabs, GWLP_WNDPROC, tab_proc as *const () as usize as isize);
    TAB_ORIG[slot].store(orig, Ordering::Relaxed);
    if slot != 0 {
        return; // the pane controls are made in the first dock and moved from there
    }
    make(
        w!("SysTreeView32"),
        WS_TABSTOP | WINDOW_STYLE(TVS_HASLINES | TVS_LINESATROOT | TVS_HASBUTTONS | TVS_SHOWSELALWAYS),
        WS_EX_CLIENTEDGE,
        ID_TREE,
    );
    make(
        w!("EDIT"),
        WS_VSCROLL | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32),
        WS_EX_CLIENTEDGE,
        ID_EVENTS,
    );
    make(
        w!("EDIT"),
        WS_VSCROLL | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32),
        WS_EX_CLIENTEDGE,
        ID_OUTPUT,
    );
    for p in layouts::PANES {
        if !is_list_pane(p) {
            continue;
        }
        let list = make(
            WC_LISTVIEWW,
            WS_TABSTOP | WINDOW_STYLE(LVS_REPORT | LVS_OWNERDATA | LVS_SHOWSELALWAYS | LVS_SINGLESEL),
            WS_EX_CLIENTEDGE,
            ID_LIST_BASE + p as usize,
        );
        send(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize);
        for (i, (name, width)) in columns(p).iter().enumerate() {
            let mut t = wide(name);
            let mut c = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM,
                cx: px(*width),
                pszText: PWSTR(t.as_mut_ptr()),
                iSubItem: i as i32,
                ..Default::default()
            };
            send(list, LVM_INSERTCOLUMNW, i, &mut c as *mut _ as isize);
        }
        if let Some(w) = app::saved_columns(&column_key(p), columns(p).len()) {
            crate::listcols::apply(list, &w);
        }
        // the filter box above the list, with the text it had when the program was last closed
        let filter = make(w!("EDIT"), WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32), WS_EX_CLIENTEDGE, filter_id(p));
        let cue = wide(&match p {
            Pane::Users => "Filter users (nick, server, channel)".to_owned(),
            _ => format!("Filter {}", p.title().to_lowercase()),
        });
        SendMessageW(filter, EM_SETCUEBANNER, WPARAM(1), LPARAM(cue.as_ptr() as isize));
        let saved = app::saved_filter(&filter_key(p));
        if !saved.is_empty() {
            LOADING_FILTERS.store(true, Ordering::Relaxed);
            let t = wide(&saved);
            let _ = SetWindowTextW(filter, PCWSTR(t.as_ptr()));
            LOADING_FILTERS.store(false, Ordering::Relaxed);
        }
    }
}

/// The settings key for the column widths of a list pane.
fn column_key(p: Pane) -> String {
    format!("dock.{}", p.title().to_ascii_lowercase())
}

/// Remember the column widths of every list pane (as the program closes).
pub fn remember_columns() {
    for p in layouts::PANES {
        if is_list_pane(p) && !list_of(p).0.is_null() {
            app::remember_columns(&column_key(p), &crate::listcols::read(list_of(p), columns(p).len()));
        }
    }
}

/// The strips of a dock window, in its client coordinates: the splitter on the side facing the chat windows,
/// the divider on the trailing side when this is the first half of a split dock, and what is left for the
/// tabs and the pane.
fn strips(hwnd: HWND) -> (RECT, Option<RECT>, layouts::Rect4) {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    let s = slot_of(hwnd);
    let (e, g) = (layouts::slot_edge(s), layouts::slot_half(s));
    let (grip, body) = layouts::split((0, 0, rc.right, rc.bottom), e.opposite(), px(GRIP), 0);
    let to_rect = |(x, y, w, h): layouts::Rect4| RECT { left: x, top: y, right: x + w, bottom: y + h };
    if g == 0 && MASK[layouts::slot(e, 1)].load(Ordering::Relaxed) != 0 {
        let trailing = if e.horizontal() { layouts::Edge::Right } else { layouts::Edge::Bottom };
        let (divider, rest) = layouts::split(body, trailing, px(GRIP), 0);
        (to_rect(grip), Some(to_rect(divider)), rest)
    } else {
        (to_rect(grip), None, body)
    }
}

unsafe fn layout(hwnd: HWND) {
    if hwnd.0.is_null() {
        return;
    }
    // The panes fill the dock except for the splitter strips.
    let s = slot_of(hwnd);
    let (_, _, (bx, by, body_w, body_h)) = strips(hwnd);
    // The tab strip wraps onto several rows when the dock is narrow; ask the control how tall.
    let tabs = tabs_of(s);
    let _ = MoveWindow(tabs, bx, by, body_w, px(24), true);
    let rows = send(tabs, TCM_GETROWCOUNT, 0, 0).max(1) as i32;
    let th = (rows * px(22) + px(4)).min(body_h);
    let _ = MoveWindow(tabs, bx, by, body_w, th, true);
    let (y, bh) = (by + th, (body_h - th).max(0));
    let mine = MASK[s].load(Ordering::Relaxed);
    for p in layouts::PANES {
        if mine & p.bit() != 0 {
            place_pane(p, bx, y, body_w, bh);
        }
    }
}

/// Is the mouse over a splitter strip of this dock window: `Some(false)` the one that sizes the dock,
/// `Some(true)` the divider between the two halves.
fn over_grip(hwnd: HWND) -> Option<bool> {
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = ScreenToClient(hwnd, &mut pt);
        let inside = |g: RECT| pt.x >= g.left && pt.x < g.right && pt.y >= g.top && pt.y < g.bottom;
        let (grip, divider, _) = strips(hwnd);
        if inside(grip) {
            Some(false)
        } else if divider.is_some_and(inside) {
            Some(true)
        } else {
            None
        }
    }
}

/// Fill in a virtual list view's request for a cell.
unsafe fn disp_info(p: Pane, di: &mut NMLVDISPINFOW) {
    if di.item.mask.0 & LVIF_TEXT.0 == 0 || di.item.pszText.0.is_null() || di.item.cchTextMax <= 0 {
        return;
    }
    let text = row_at(p, di.item.iItem)
        .and_then(|r| r.cells.get(di.item.iSubItem as usize).cloned())
        .unwrap_or_default();
    let units: Vec<u16> = text.encode_utf16().take(di.item.cchTextMax as usize - 1).collect();
    let buf = std::slice::from_raw_parts_mut(di.item.pszText.0, di.item.cchTextMax as usize);
    buf[..units.len()].copy_from_slice(&units);
    buf[units.len()] = 0;
}

/// Notifications from the pane controls (the server tree and the list panes), wherever they are: in a
/// dock or in a floating window.
unsafe fn pane_notify(nm: &NMHDR, lp: LPARAM) -> LRESULT {
        if nm.idFrom == ID_TREE {
            if nm.code == NM_DBLCLK || nm.code == NM_RETURN {
                activate_selected();
                // handled: the default would also expand/collapse on a double click
                return LRESULT(1);
            } else if nm.code == NM_RCLICK {
                tree_context_menu();
                return LRESULT(1);
            } else if nm.code == TVN_KEYDOWN {
                let k = &*(lp.0 as *const NMTVKEYDOWN);
                if k.wVKey == VK_DELETE.0 {
                    if let Some(s) = selected_server() {
                        app::server_list_remove(&s);
                    }
                }
            }
        } else if nm.idFrom >= ID_LIST_BASE {
            if let Some(p) = Pane::from_index((nm.idFrom - ID_LIST_BASE) as u8).filter(|p| is_list_pane(*p)) {
                if nm.code == LVN_GETDISPINFOW {
                    disp_info(p, &mut *(lp.0 as *mut NMLVDISPINFOW));
                } else if nm.code == NM_DBLCLK || nm.code == NM_RETURN {
                    activate_row(p);
                } else if nm.code == NM_RCLICK {
                    list_context_menu(p);
                } else if nm.code == LVN_KEYDOWN {
                    let k = &*(lp.0 as *const NMLVKEYDOWN);
                    if k.wVKey == VK_DELETE.0 && matches!(p, Pane::Messages | Pane::Notices | Pane::Urls) {
                        clear(p);
                    } else if k.wVKey == VK_DELETE.0 && p == Pane::Buddies {
                        if let Some(Row { target: Target::Buddy { nick, .. }, .. }) = selected_row(p) {
                            app::buddy_remove_from_dock(&nick);
                        }
                    }
                }
            }
        }
    LRESULT(0)
}

extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                let cs = &*(lp.0 as *const CREATESTRUCTW);
                let slot = (cs.lpCreateParams as usize).min(7);
                DOCKS[slot].store(hwnd.0 as isize, Ordering::Relaxed);
                create_controls(hwnd, slot);
                LRESULT(0)
            }
            WM_SIZE => {
                layout(hwnd);
                LRESULT(0)
            }
            WM_COMMAND => {
                on_command(wp, lp);
                LRESULT(0)
            }
            WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC | WM_CTLCOLORLISTBOX => control_colors(wp),
            WM_DRAWITEM => {
                let dis = &*(lp.0 as *const DRAWITEMSTRUCT);
                let s = slot_of(hwnd);
                if dis.hwndItem == tabs_of(s) {
                    draw_dock_tab(s, dis);
                    return LRESULT(1);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let (grip, divider, _) = strips(hwnd);
                let horizontal = edge_of(hwnd).horizontal();
                let sides = if horizontal { BF_TOP | BF_BOTTOM } else { BF_LEFT | BF_RIGHT };
                let t = crate::look::theme();
                // the etched edge is white and grey: draw plain lines in the border colour instead
                let line = CreateSolidBrush(crate::look::colorref(t.border));
                let mut strips = vec![(grip, sides)];
                if let Some(d) = divider {
                    strips.push((d, if horizontal { BF_LEFT | BF_RIGHT } else { BF_TOP | BF_BOTTOM }));
                }
                for (r, s) in strips {
                    let fill = |l, tp, rt, b| {
                        let q = RECT { left: l, top: tp, right: rt, bottom: b };
                        FillRect(hdc, &q, line);
                    };
                    if s.0 & BF_TOP.0 != 0 { fill(r.left, r.top, r.right, r.top + 1); }
                    if s.0 & BF_BOTTOM.0 != 0 { fill(r.left, r.bottom - 1, r.right, r.bottom); }
                    if s.0 & BF_LEFT.0 != 0 { fill(r.left, r.top, r.left + 1, r.bottom); }
                    if s.0 & BF_RIGHT.0 != 0 { fill(r.right - 1, r.top, r.right, r.bottom); }
                }
                let _ = DeleteObject(HGDIOBJ(line.0));
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_SETCURSOR => {
                let s = slot_of(hwnd) as i32;
                let kind = if DRAGGING.load(Ordering::Relaxed) == s { Some(DRAG_DIVIDER.load(Ordering::Relaxed)) } else { over_grip(hwnd) };
                if let Some(divider) = kind {
                    // the divider runs across the dock's short side: it moves along the long one
                    let along_x = edge_of(hwnd).horizontal() == divider;
                    let cursor = if along_x { IDC_SIZEWE } else { IDC_SIZENS };
                    SetCursor(LoadCursorW(None, cursor).unwrap_or_default());
                    return LRESULT(1);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_LBUTTONDOWN => {
                if let Some(divider) = over_grip(hwnd) {
                    DRAGGING.store(slot_of(hwnd) as i32, Ordering::Relaxed);
                    DRAG_DIVIDER.store(divider, Ordering::Relaxed);
                    SetCapture(hwnd);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if DRAGGING.load(Ordering::Relaxed) == slot_of(hwnd) as i32 {
                    let mut cur = POINT::default();
                    let _ = GetCursorPos(&mut cur);
                    let e = edge_of(hwnd);
                    if DRAG_DIVIDER.load(Ordering::Relaxed) {
                        // the divider: the first half takes the share between the dock's start and the mouse
                        let (mut a, mut b) = (RECT::default(), RECT::default());
                        let _ = GetWindowRect(handle_slot(layouts::slot(e, 0)), &mut a);
                        let _ = GetWindowRect(handle_slot(layouts::slot(e, 1)), &mut b);
                        let (start, total, at) = if e.horizontal() { (a.left, b.right - a.left, cur.x) } else { (a.top, b.bottom - a.top, cur.y) };
                        if total > 0 {
                            let pct = ((at - start) * 100 / total).clamp(layouts::SPLIT_MIN as i32, layouts::SPLIT_MAX as i32);
                            SPLIT[e as usize].store(pct, Ordering::Relaxed);
                            ui::relayout();
                        }
                    } else {
                        // The edge of the dock that touches the window border stays where it is; the size is the
                        // distance from there to the mouse.
                        let mut dr = RECT::default();
                        let _ = GetWindowRect(hwnd, &mut dr);
                        let mut fr = RECT::default();
                        let _ = GetClientRect(ui::frame(), &mut fr);
                        let (wanted, frame_len) = match e {
                            layouts::Edge::Left => (cur.x - dr.left, fr.right),
                            layouts::Edge::Right => (dr.right - cur.x, fr.right),
                            layouts::Edge::Top => (cur.y - dr.top, fr.bottom),
                            layouts::Edge::Bottom => (dr.bottom - cur.y, fr.bottom),
                        };
                        // The dock may take up to all but 200 device pixels of the window.
                        let width = wanted.clamp(px(MIN_WIDTH), px(MAX_WIDTH).min((frame_len - px(200)).max(px(MIN_WIDTH))));
                        let dpi = px(96).max(1);
                        SIZE96[e as usize].store(width * 96 / dpi, Ordering::Relaxed);
                        ui::relayout();
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if DRAGGING.load(Ordering::Relaxed) == slot_of(hwnd) as i32 {
                    DRAGGING.store(-1, Ordering::Relaxed);
                    let _ = ReleaseCapture();
                    app::dock_resized();
                }
                LRESULT(0)
            }
            WM_NOTIFY => {
                let nm = &*(lp.0 as *const NMHDR);
                if nm.idFrom == ID_TABS && nm.code == TCN_SELCHANGE {
                    let s = slot_of(hwnd);
                    let i = send(tabs_of(s), TCM_GETCURSEL, 0, 0).max(0) as usize;
                    if let Some(p) = TAB_PANES.with(|t| t.borrow()[s].get(i).copied()) {
                        ACTIVE[s].store(p as u8, Ordering::Relaxed);
                        show_pane_controls();
                        app::dock_changed();
                    }
                } else if nm.idFrom == ID_TABS && nm.code == NM_RCLICK {
                    tab_context_menu(slot_of(hwnd));
                    return LRESULT(1);
                } else {
                    return pane_notify(nm, lp);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// The window procedure of the windows that floating panes live in.
extern "system" fn float_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SIZE => {
                float_layout(hwnd);
                LRESULT(0)
            }
            WM_COMMAND => {
                on_command(wp, lp);
                LRESULT(0)
            }
            WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC | WM_CTLCOLORLISTBOX => control_colors(wp),
            WM_EXITSIZEMOVE => {
                app::dock_resized();
                LRESULT(0)
            }
            // closing the window switches the pane's tab off, like Hide Tab
            WM_CLOSE => {
                if let Some(p) = float_pane_of(hwnd) {
                    app::dock_pane_switch(p as usize);
                }
                LRESULT(0)
            }
            // right click on the title bar: put the pane back into a dock
            WM_CONTEXTMENU if DefWindowProcW(hwnd, WM_NCHITTEST, WPARAM(0), lp).0 as u32 == HTCAPTION => {
                if let Some(p) = float_pane_of(hwnd) {
                    let menu = CreatePopupMenu().unwrap_or_default();
                    for to in layouts::EDGES {
                        let t = wide(&format!("Dock to {}", to.title()));
                        let _ = AppendMenuW(menu, MF_STRING, MENU_TO_EDGE + to as usize, PCWSTR(t.as_ptr()));
                    }
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None).0 as usize;
                    let _ = DestroyMenu(menu);
                    if (MENU_TO_EDGE..MENU_TO_EDGE + 4).contains(&cmd) {
                        app::dock_move_pane(p as usize, cmd - MENU_TO_EDGE);
                    }
                }
                LRESULT(0)
            }
            WM_NOTIFY => pane_notify(&*(lp.0 as *const NMHDR), lp),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// The subclass procedure of a dock's tab strip: a tab can be dragged. Letting go over a dock moves the
/// pane there, near a window edge makes a dock on that edge, anywhere else makes it float.
unsafe extern "system" fn tab_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let slot = (0..8usize).find(|s| tabs_of(*s) == hwnd).unwrap_or(0);
    let orig = TAB_ORIG[slot].load(Ordering::Relaxed);
    let call = || CallWindowProcW(std::mem::transmute::<isize, WNDPROC>(orig), hwnd, msg, wp, lp);
    let (x, y) = ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32);
    match msg {
        WM_LBUTTONDOWN => {
            let mut hit = TCHITTESTINFO { pt: POINT { x, y }, ..Default::default() };
            let i = send(hwnd, TCM_HITTEST, 0, &mut hit as *mut _ as isize);
            let pane = (i >= 0).then(|| TAB_PANES.with(|t| t.borrow()[slot].get(i as usize).copied())).flatten();
            TAB_DRAG.with(|d| d.set(None));
            // the click selects the tab, which rebuilds the strip and may cancel a capture: arm the drag after
            let r = call();
            if let Some(pane) = pane {
                TAB_DRAG.with(|d| d.set(Some(TabDrag { slot, pane, x, y, active: false })));
                // keep receiving the mouse when it leaves the strip, and watch for Escape
                SetCapture(hwnd);
                SetTimer(hwnd, TAB_TIMER, 40, None);
            }
            r
        }
        WM_TIMER if wp.0 == TAB_TIMER => {
            if escape_pressed_during_drag() {
                cancel_tab_drag(hwnd);
            } else if TAB_DRAG.with(|d| d.get()).is_none() {
                let _ = KillTimer(hwnd, TAB_TIMER);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if escape_pressed_during_drag() {
                cancel_tab_drag(hwnd);
                return call();
            }
            if let Some(mut d) = TAB_DRAG.with(|d| d.get()) {
                if wp.0 & 1 == 0 {
                    TAB_DRAG.with(|d| d.set(None)); // the button is no longer down
                } else {
                    if !d.active {
                        let far = GetSystemMetrics(SM_CXDRAG).max(4);
                        if (x - d.x).abs() > far || (y - d.y).abs() > far {
                            d.active = true;
                            TAB_DRAG.with(|c| c.set(Some(d)));
                        }
                    }
                    if d.active {
                        SetCursor(LoadCursorW(None, IDC_SIZEALL).unwrap_or_default());
                        let mut cur = POINT::default();
                        let _ = GetCursorPos(&mut cur);
                        match plan_drop(&d, cur) {
                            Plan::Place { hint, .. } | Plan::Float { hint, .. } => show_hint(hint),
                            Plan::Nothing => hide_hint(),
                        }
                        return LRESULT(0);
                    }
                }
            }
            call()
        }
        WM_SETCURSOR if TAB_DRAG.with(|d| d.get()).is_some_and(|d| d.active) => {
            SetCursor(LoadCursorW(None, IDC_SIZEALL).unwrap_or_default());
            LRESULT(1)
        }
        WM_LBUTTONUP => {
            let _ = KillTimer(hwnd, TAB_TIMER);
            if let Some(d) = TAB_DRAG.with(|d| d.take()) {
                let _ = ReleaseCapture();
                hide_hint();
                if d.active {
                    finish_tab_drag(d);
                    return LRESULT(0);
                }
            }
            call()
        }
        WM_CAPTURECHANGED | WM_CANCELMODE => {
            TAB_DRAG.with(|d| d.set(None));
            let _ = KillTimer(hwnd, TAB_TIMER);
            hide_hint();
            call()
        }
        _ => call(),
    }
}

/// Is a tab being dragged (past the first few pixels) while Escape is held down?
fn escape_pressed_during_drag() -> bool {
    TAB_DRAG.with(|d| d.get()).is_some_and(|d| d.active) && unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) } as u16 & 0x8000 != 0
}

/// Give up the drag: nothing moves, the marker goes away. (The button may still be down; its release is
/// then an ordinary click on the tab strip.)
unsafe fn cancel_tab_drag(hwnd: HWND) {
    TAB_DRAG.with(|d| d.set(None));
    let _ = KillTimer(hwnd, TAB_TIMER);
    let _ = ReleaseCapture();
    hide_hint();
}

/// What letting go of a dragged tab here would do, and the rectangle (screen coordinates) to show for it.
enum Plan {
    Nothing,
    /// Into half `group` of the dock on `edge`, at tab position `index` when over its tab strip.
    Place { edge: layouts::Edge, group: usize, index: Option<usize>, hint: RECT },
    Float { x: i32, y: i32, hint: RECT },
}

/// The window rectangle of dock half `s` in screen coordinates, when that half is shown.
unsafe fn slot_rect(s: usize) -> Option<RECT> {
    let h = handle_slot(s);
    if h.0.is_null() || MASK[s].load(Ordering::Relaxed) == 0 || size_px(layouts::slot_edge(s)) == 0 {
        return None;
    }
    let mut r = RECT::default();
    let _ = GetWindowRect(h, &mut r);
    Some(r)
}

/// The main window's client area and the shown docks (both halves together), in screen coordinates.
unsafe fn screen_geometry() -> (layouts::Rect4, [Option<layouts::Rect4>; 4]) {
    let frame = ui::frame();
    let mut rc = RECT::default();
    let _ = GetClientRect(frame, &mut rc);
    let mut origin = POINT::default();
    let _ = ClientToScreen(frame, &mut origin);
    let docks = layouts::EDGES.map(|e| {
        let rects: Vec<RECT> = (0..2).filter_map(|g| slot_rect(layouts::slot(e, g))).collect();
        let (l, t) = (rects.iter().map(|r| r.left).min()?, rects.iter().map(|r| r.top).min()?);
        let (r, b) = (rects.iter().map(|r| r.right).max()?, rects.iter().map(|r| r.bottom).max()?);
        Some((l, t, r - l, b - t))
    });
    ((origin.x, origin.y, rc.right, rc.bottom), docks)
}

fn rect_of((x, y, w, h): layouts::Rect4) -> RECT {
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

/// If `cur` is over the tab strip of dock half `s`: where a dragged tab would be inserted (its position
/// among the other tabs) and a thin bar at that place.
unsafe fn tab_slot(s: usize, cur: POINT, dragged: Pane) -> Option<(usize, RECT)> {
    let tabs = tabs_of(s);
    if tabs.0.is_null() || slot_rect(s).is_none() {
        return None;
    }
    let mut strip = RECT::default();
    let _ = GetWindowRect(tabs, &mut strip);
    if cur.x < strip.left || cur.x >= strip.right || cur.y < strip.top || cur.y >= strip.bottom {
        return None;
    }
    let count = send(tabs, TCM_GETITEMCOUNT, 0, 0).max(0) as usize;
    let mut hit = TCHITTESTINFO { pt: POINT { x: cur.x - strip.left, y: cur.y - strip.top }, ..Default::default() };
    let over = send(tabs, TCM_HITTEST, 0, &mut hit as *mut _ as isize);
    let item = |i: usize| {
        let mut r = RECT::default();
        send(tabs, TCM_GETITEMRECT, i, &mut r as *mut _ as isize);
        r
    };
    // the slot (a position among all tabs, the dragged one included): before the tab under the mouse when
    // the mouse is in its left half, after it otherwise, at the end when over no tab
    let (slot, x, top, bottom) = if over >= 0 {
        let r = item(over as usize);
        let before = hit.pt.x < (r.left + r.right) / 2;
        let slot = over as usize + usize::from(!before);
        (slot, strip.left + if before { r.left } else { r.right }, strip.top + r.top, strip.top + r.bottom)
    } else if count > 0 {
        let r = item(count - 1);
        (count, strip.left + r.right, strip.top + r.top, strip.top + r.bottom)
    } else {
        (0, strip.left + px(2), strip.top, strip.bottom)
    };
    // the number of tabs before the slot, not counting the dragged tab itself
    let order = TAB_PANES.with(|t| t.borrow()[s].clone());
    let before_slot = order.iter().take(slot).filter(|p| **p != dragged).count();
    let bar = px(3);
    Some((before_slot, RECT { left: x - bar / 2, top, right: x - bar / 2 + bar, bottom }))
}

unsafe fn plan_drop(d: &TabDrag, cur: POINT) -> Plan {
    let (frame, docks) = screen_geometry();
    match layouts::drop_target((cur.x, cur.y), frame, docks, px(48)) {
        layouts::DropAt::Dock(e) => {
            for g in 0..2 {
                if let Some((index, bar)) = tab_slot(layouts::slot(e, g), cur, d.pane) {
                    return Plan::Place { edge: e, group: g, index: Some(index), hint: bar };
                }
            }
            // the half under the mouse
            let g = usize::from(slot_rect(layouts::slot(e, 1)).is_some_and(|r| cur.x >= r.left && cur.x < r.right && cur.y >= r.top && cur.y < r.bottom));
            if layouts::slot(e, g) == d.slot {
                return Plan::Nothing;
            }
            // a dock half that is there is lit up whole; for a new dock, the strip it would take
            let hint = match slot_rect(layouts::slot(e, g)) {
                Some(r) => r,
                None => {
                    let keep = if e.horizontal() { px(140) } else { px(100) };
                    rect_of(layouts::split(frame, e, px(e.default_size() as i32), keep).0)
                }
            };
            Plan::Place { edge: e, group: g, index: None, hint }
        }
        layouts::DropAt::Float(x, y) => {
            let (left, top) = (x - px(150), y - px(10));
            Plan::Float { x: left, y: top, hint: rect_of((left, top, px(360), px(300))) }
        }
    }
}

/// Show the translucent drop marker over `r` (screen coordinates).
unsafe fn show_hint(r: RECT) {
    let mut h = HWND(HINT.load(Ordering::Relaxed) as *mut c_void);
    if h.0.is_null() {
        HINT_REGISTERED.call_once(|| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(hint_proc),
                hInstance: ui::hinstance(),
                lpszClassName: HINT_CLASS,
                hbrBackground: HBRUSH(GetSysColorBrush(COLOR_HIGHLIGHT).0),
                ..Default::default()
            };
            RegisterClassW(&wc);
        });
        h = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            HINT_CLASS,
            PCWSTR::null(),
            WS_POPUP,
            0, 0, 0, 0,
            ui::frame(), None, ui::hinstance(), None,
        )
        .unwrap_or_default();
        if h.0.is_null() {
            return;
        }
        let _ = SetLayeredWindowAttributes(h, windows::Win32::Foundation::COLORREF(0), 110, LWA_ALPHA);
        HINT.store(h.0 as isize, Ordering::Relaxed);
    }
    let _ = SetWindowPos(h, HWND_TOPMOST, r.left, r.top, (r.right - r.left).max(2), (r.bottom - r.top).max(2), SWP_NOACTIVATE | SWP_SHOWWINDOW);
}

fn hide_hint() {
    let h = HWND(HINT.load(Ordering::Relaxed) as *mut c_void);
    if !h.0.is_null() {
        unsafe {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
}

extern "system" fn hint_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// A dragged tab was let go: do what the marker showed.
fn finish_tab_drag(d: TabDrag) {
    unsafe {
        let mut cur = POINT::default();
        let _ = GetCursorPos(&mut cur);
        match plan_drop(&d, cur) {
            Plan::Place { edge, group, index, .. } => app::dock_place_pane(d.pane as usize, edge as usize, group, index),
            Plan::Float { x, y, .. } => app::dock_float_pane(d.pane as usize, x, y),
            Plan::Nothing => {}
        }
    }
}
