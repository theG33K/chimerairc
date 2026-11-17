//! Win32 front end: MDI frame, tab bar, status bar and the server/channel/query child windows.

use crate::app;
use crate::complete;
use crate::dock;
use crate::layouts::{self, Layout};
use crate::highlight;
use crate::util::*;
use chimerairc_proto::format::{self, Rgb, Style};
use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Controls::RichEdit::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_EVENT: u32 = WM_APP + 1;
/// Posted to a chat window after the pointer moved over its text: is it still over a link?
const WM_HOVER_CHECK: u32 = WM_APP + 7;

const FIRST_CHILD: u32 = 50000;

pub const ID_FILE_NEW: usize = 100;
pub const ID_FILE_CLOSE: usize = 101;
pub const ID_FILE_DISCONNECT: usize = 102;
pub const ID_APP_EXIT: usize = 103;
pub const ID_FILE_CONNECT: usize = 104;
pub const ID_CHANNEL_JOIN: usize = 105;
pub const ID_CHANNEL_LIST: usize = 106;
const ID_FILE_IMPORT: usize = 109;
const ID_FILE_DCC: usize = 160; // (110-113 are the Edit menu)
const ID_FAV_ADD: usize = 107;
const ID_FAV_ORGANIZE: usize = 108;
/// First command id handed to dynamic Favorites menu items.
const FAV_BASE: usize = 3000;
/// Items in the Favorites menu before the dynamic part (Add, Organize, separator).
const FAV_STATIC_ITEMS: i32 = 3;
pub const ID_OPT_PREFS: usize = 150;
pub const ID_OPT_TIMESTAMPS: usize = 151;
pub const ID_OPT_USERLIST: usize = 152;
pub const ID_OPT_HIGHLIGHTS: usize = 153;
pub const ID_OPT_LOGGING: usize = 154;
const ID_OPT_IGNORES: usize = 157;
const ID_OPT_PERFORM: usize = 158;
/// Options > Theme: light, dark, follow Windows (three consecutive ids), then the colours and the fonts.
const ID_OPT_THEME: usize = 170;
const ID_OPT_COLORS: usize = 173;
const ID_OPT_CHATFONT: usize = 174;
const ID_OPT_UIFONT: usize = 175;
const ID_OPT_TYPING_SHOW: usize = 176;
const ID_OPT_IDENTITIES: usize = 178;
const ID_OPT_ALIASES: usize = 179;
const ID_EDIT_FIND: usize = 114;
const ID_EDIT_FINDNEXT: usize = 115;
const ID_EDIT_FINDPREV: usize = 116;
const ID_EDIT_LOGSEARCH: usize = 117;
const ID_OPT_TYPING_SEND: usize = 177;
/// Once a second: pauses in our typing, and other people's typing that ran out.
const TYPING_TIMER: usize = 0x7701;
/// Pasted lines go out one per tick.
const PASTE_TIMER: usize = 0x7702;
const ID_OPT_OPENLOGS: usize = 155;
const ID_OPT_OPENLOG: usize = 156;
const ID_VIEW_TOOLBAR: usize = 121;
const ID_VIEW_SERVERS: usize = 122;
const ID_VIEW_EVENTS: usize = 123;
/// View > Panes: `ID_PANE_BASE + pane index`; View > Layouts: load / save `+ slot`.
const ID_PANE_BASE: usize = 200;
const ID_LAYOUT_LOAD: usize = 210;
const ID_LAYOUT_SAVE: usize = 220;
/// View > Dock Edge: `ID_DOCK_EDGE + edge index`.
const ID_DOCK_EDGE: usize = 230;
const ID_WIN_MINALL: usize = 134;

// User-list context menu commands (returned by TrackPopupMenu, never routed as WM_COMMAND).
const UM_QUERY: usize = 2001;
const UM_WHOIS: usize = 2002;
const UM_CTCP_VERSION: usize = 2003;
const UM_CTCP_PING: usize = 2004;
const UM_CTCP_TIME: usize = 2005;
const UM_OP: usize = 2010;
const UM_DEOP: usize = 2011;
const UM_VOICE: usize = 2012;
const UM_DEVOICE: usize = 2013;
const UM_KICK: usize = 2014;
const UM_BAN: usize = 2015;
const UM_KICKBAN: usize = 2016;
const UM_COPY: usize = 2020;
const UM_SENDFILE: usize = 2006;
const UM_DCCCHAT: usize = 2007;
const UM_IGNORE: usize = 2030;
const UM_UNIGNORE: usize = 2031;
const UM_BUDDY_ADD: usize = 2032;
const UM_BUDDY_DEL: usize = 2033;

/// What the user picked from the user-list context menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UserAction {
    Query,
    Whois,
    CtcpVersion,
    CtcpPing,
    CtcpTime,
    Op,
    Deop,
    Voice,
    Devoice,
    Kick,
    Ban,
    KickBan,
    CopyNick,
    Ignore,
    Unignore,
    SendFile,
    DccChat,
    AddBuddy,
    RemoveBuddy,
}
const ID_EDIT_CUT: usize = 110;
const ID_EDIT_COPY: usize = 111;
const ID_EDIT_PASTE: usize = 112;
const ID_EDIT_CLEAR: usize = 113;
const ID_VIEW_STATUS: usize = 120;
const ID_WIN_CASCADE: usize = 130;
const ID_WIN_TILEH: usize = 131;
const ID_WIN_TILEV: usize = 132;
const ID_WIN_ARRANGE: usize = 133;
const ID_HELP_ABOUT: usize = 140;

static FRAME: AtomicIsize = AtomicIsize::new(0);
static MDI: AtomicIsize = AtomicIsize::new(0);
static TAB: AtomicIsize = AtomicIsize::new(0);
static STATUS: AtomicIsize = AtomicIsize::new(0);
static HINST: AtomicIsize = AtomicIsize::new(0);
static BOLD_FONT: AtomicIsize = AtomicIsize::new(0);
static TAB_ICONS: AtomicIsize = AtomicIsize::new(0);
static UI_FONT: AtomicIsize = AtomicIsize::new(0);
static MONO_FONT: AtomicIsize = AtomicIsize::new(0);
static MENU_BAR: AtomicIsize = AtomicIsize::new(0);
static FAV_MENU: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    /// Dynamic Favorites items: command id - FAV_BASE indexes this: (server index, channel index).
    static FAV_ACTIONS: RefCell<Vec<(usize, Option<usize>)>> = const { RefCell::new(Vec::new()) };
}

/// One favorite server as shown in the menu.
pub struct FavMenuServer {
    pub label: String,
    pub channels: Vec<String>,
}

/// Which favorite a dynamic menu command refers to.
fn favorite_for_command(id: usize) -> Option<(usize, Option<usize>)> {
    id.checked_sub(FAV_BASE).and_then(|i| FAV_ACTIONS.with(|a| a.borrow().get(i).copied()))
}

/// Rebuild the dynamic part of the Favorites menu. Servers with channels become submenus
/// ("Connect and join all" plus one item per channel); servers without are a single item.
pub fn set_favorites_menu(servers: &[FavMenuServer]) {
    let menu = HMENU(FAV_MENU.load(Ordering::Relaxed) as *mut c_void);
    if menu.0.is_null() {
        return;
    }
    // Menu text: '&' would become a mnemonic marker.
    let text = |s: &str| wide(&s.replace('&', "&&"));
    unsafe {
        while GetMenuItemCount(menu) > FAV_STATIC_ITEMS {
            let _ = DeleteMenu(menu, FAV_STATIC_ITEMS as u32, MF_BYPOSITION);
        }
        let mut actions = Vec::new();
        let mut next_id = FAV_BASE;
        let mut alloc = |action: (usize, Option<usize>)| {
            actions.push(action);
            next_id += 1;
            next_id - 1
        };
        if servers.is_empty() {
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("(no favorites yet)"));
        }
        for (si, s) in servers.iter().enumerate() {
            let label = text(&s.label);
            if s.channels.is_empty() {
                let id = alloc((si, None));
                let _ = AppendMenuW(menu, MF_STRING, id, PCWSTR(label.as_ptr()));
                continue;
            }
            let Ok(sub) = CreatePopupMenu() else { continue };
            let all = text(&format!("Connect and join all ({})", s.channels.len()));
            let id = alloc((si, None));
            let _ = AppendMenuW(sub, MF_STRING, id, PCWSTR(all.as_ptr()));
            let _ = AppendMenuW(sub, MF_SEPARATOR, 0, PCWSTR::null());
            for (ci, c) in s.channels.iter().enumerate() {
                let id = alloc((si, Some(ci)));
                let t = text(c);
                let _ = AppendMenuW(sub, MF_STRING, id, PCWSTR(t.as_ptr()));
            }
            let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, PCWSTR(label.as_ptr()));
        }
        FAV_ACTIONS.with(|a| *a.borrow_mut() = actions);
        let frame = frame();
        if !frame.0.is_null() {
            let _ = DrawMenuBar(frame);
        }
    }
}
static TOOLBAR: AtomicIsize = AtomicIsize::new(0);
static SHOW_TOOLBAR: AtomicBool = AtomicBool::new(true);
static SHOW_USERLIST: AtomicBool = AtomicBool::new(true);
static LOGGING: AtomicBool = AtomicBool::new(false);
static WINDOW_MENU: AtomicIsize = AtomicIsize::new(0);

const CHILD_CLASS: PCWSTR = w!("ChimeraIRC.Child");
const FRAME_CLASS: PCWSTR = w!("ChimeraIRC.Frame");

pub fn frame() -> HWND {
    hw(FRAME.load(Ordering::Relaxed))
}
fn mdi() -> HWND {
    hw(MDI.load(Ordering::Relaxed))
}
fn tab() -> HWND {
    hw(TAB.load(Ordering::Relaxed))
}
fn status() -> HWND {
    hw(STATUS.load(Ordering::Relaxed))
}
pub fn hinstance() -> windows::Win32::Foundation::HINSTANCE {
    hinst()
}
pub fn ui_font_raw() -> isize {
    UI_FONT.load(Ordering::Relaxed)
}
fn hinst() -> windows::Win32::Foundation::HINSTANCE {
    windows::Win32::Foundation::HINSTANCE(HINST.load(Ordering::Relaxed) as *mut c_void)
}
fn ui_font() -> WPARAM {
    WPARAM(UI_FONT.load(Ordering::Relaxed) as usize)
}
fn mono_font() -> WPARAM {
    WPARAM(MONO_FONT.load(Ordering::Relaxed) as usize)
}

fn send(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    unsafe { SendMessageW(h, msg, WPARAM(wp), LPARAM(lp)).0 }
}

// ---------------------------------------------------------------- child state

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Server,
    Channel,
    Query,
    /// A DCC chat with one person (`name` is `=nick`).
    DccChat,
}

pub struct Child {
    pub kind: Kind,
    pub conn: u32,
    /// DCC transfer id for a [`Kind::DccChat`] window (0 otherwise).
    pub dcc: u32,
    /// Target (channel/nick) for Channel/Query; display name for Server.
    pub name: String,
    pub output: HWND,
    pub input: HWND,
    pub topic: HWND,
    pub list: HWND,
    /// (prefix symbol, nick) in list order.
    pub members: Vec<(String, String)>,
    /// Nicks (lower case) that `away-notify` says are away; shown as such in the nick list.
    pub away: std::collections::HashSet<String>,
    /// What we told the other end about our entry box, and who is typing to us here (the `+typing` tag).
    typing_out: crate::typing::Outgoing,
    typing_in: crate::typing::Incoming,
    /// Unread activity shown on this window's tab; cleared when the window is activated.
    pub activity: Activity,
    /// Nicks that spoke here, most recent first (completion prefers them).
    recent: Vec<String>,
    completion: Option<complete::Completion>,
    history: Vec<String>,
    hist_pos: usize,
    /// The lines printed in this window (the last ones), to print them again when the colours change.
    lines: std::collections::VecDeque<StoredLine>,
}

/// One printed line as it was given to [`print`].
#[derive(Clone)]
struct StoredLine {
    ts: Option<String>,
    text: String,
    kind: LineKind,
    marks: highlight::Marks,
}

/// How many printed lines a window remembers for printing again.
const STORED_LINES: usize = 3000;

/// How interesting the unread lines in a background window are, lowest to highest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Activity {
    #[default]
    None,
    /// Joins, parts, modes and other status lines.
    Events,
    /// Chat from other people.
    Messages,
    /// A highlight rule matched (our nick or a user-defined word).
    Highlight,
}

impl From<LineKind> for Activity {
    fn from(k: LineKind) -> Self {
        match k {
            LineKind::Info => Activity::Events,
            _ => Activity::Messages,
        }
    }
}

/// Fetch per-window state. Only call with handles of our own child windows, on the UI thread.
pub fn child<'a>(h: HWND) -> Option<&'a mut Child> {
    let p = unsafe { GetWindowLongPtrW(h, GWLP_USERDATA) } as *mut Child;
    if p.is_null() {
        None
    } else {
        Some(unsafe { &mut *p })
    }
}

/// Put the keyboard focus in a child window's input line (e.g. after a modal prompt closed and
/// handed focus back to whatever had it before).
pub fn focus_input(h: HWND) {
    if let Some(c) = child(h) {
        unsafe {
            let _ = SetFocus(c.input);
        }
    }
}

/// Bring an MDI child to the front, restoring it if minimized.
pub fn activate(h: HWND) {
    if unsafe { IsIconic(h) }.as_bool() {
        send(mdi(), WM_MDIRESTORE, raw(h) as usize, 0);
    }
    send(mdi(), WM_MDIACTIVATE, raw(h) as usize, 0);
}

pub fn active_child() -> Option<HWND> {
    let h = HWND(send(mdi(), WM_MDIGETACTIVE, 0, 0) as *mut c_void);
    if h.0.is_null() {
        None
    } else {
        Some(h)
    }
}

// ---------------------------------------------------------------- init

pub fn init() -> windows::core::Result<HWND> {
    unsafe {
        let hi = GetModuleHandleW(None)?;
        HINST.store(hi.0 as isize, Ordering::Relaxed);

        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_TAB_CLASSES | ICC_BAR_CLASSES | ICC_STANDARD_CLASSES | ICC_LISTVIEW_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);
        let _ = LoadLibraryW(w!("Msftedit.dll")); // RICHEDIT50W

        let (choice, overrides, chat_font, ui_font) = app::look_settings();
        crate::look::init(choice, overrides, chat_font, ui_font);
        let ui = crate::look::make_font(&crate::look::ui_font(), false);
        let mono = crate::look::make_font(&crate::look::chat_font(), false);
        let bold = crate::look::make_font(&crate::look::ui_font(), true);
        UI_FONT.store(ui.0 as isize, Ordering::Relaxed);
        MONO_FONT.store(mono.0 as isize, Ordering::Relaxed);
        BOLD_FONT.store(bold.0 as isize, Ordering::Relaxed);
        TAB_ICONS.store(load_icons(101, 15).0 as isize, Ordering::Relaxed);

        let icon = LoadIconW(hi, PCWSTR(1 as *const u16)).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(frame_proc),
            hInstance: hi.into(),
            lpszClassName: FRAME_CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: icon,
            // Behind the toolbar and tab bar; the MDI client paints its own workspace colour.
            hbrBackground: crate::look::panel_brush(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let cc = WNDCLASSW {
            lpfnWndProc: Some(child_proc),
            hInstance: hi.into(),
            lpszClassName: CHILD_CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: icon,
            hbrBackground: crate::look::panel_brush(),
            ..Default::default()
        };
        RegisterClassW(&cc);

        let menu = build_menu()?;
        let frame = CreateWindowExW(
            WINDOW_EX_STYLE(0), FRAME_CLASS, w!("ChimeraIRC"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT, CW_USEDEFAULT, px(1024), px(700),
            None, menu, hi, None,
        )?;
        Ok(frame)
    }
}

/// The usable area of every screen, the primary one first.
pub fn screens() -> Vec<crate::winplace::Rc> {
    use crate::winplace::Rc;
    unsafe extern "system" fn collect(m: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> windows::Win32::Foundation::BOOL {
        let v = &mut *(lp.0 as *mut Vec<(bool, Rc)>);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(m, &mut mi).as_bool() {
            let w = mi.rcWork;
            v.push((mi.dwFlags & 1 != 0, Rc { l: w.left, t: w.top, r: w.right, b: w.bottom }));
        }
        true.into()
    }
    let mut found: Vec<(bool, Rc)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut found as *mut _ as isize));
    }
    found.sort_by_key(|(primary, _)| !*primary);
    found.into_iter().map(|(_, r)| r).collect()
}

/// Show the main window where it was when it was last closed (kept reachable on the screens that exist
/// now), or in the default place the first time.
pub fn show_frame(frame: HWND) {
    use crate::winplace;
    unsafe {
        let Some(saved) = app::saved_window() else {
            let _ = ShowWindow(frame, SW_SHOWDEFAULT);
            return;
        };
        let screens = screens();
        let p = winplace::fit(saved, &screens);
        let wp = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            showCmd: if p.maximized { SW_SHOWMAXIMIZED.0 as u32 } else { SW_SHOWNORMAL.0 as u32 },
            rcNormalPosition: RECT { left: p.left, top: p.top, right: p.left + p.width, bottom: p.top + p.height },
            ..Default::default()
        };
        let _ = SetWindowPlacement(frame, &wp);
    }
}

/// Record the main window's restored position and size, and whether it is maximized.
fn remember_frame(hwnd: HWND) {
    unsafe {
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        if GetWindowPlacement(hwnd, &mut wp).is_err() {
            return;
        }
        let r = wp.rcNormalPosition;
        let maximized = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32 || wp.flags.0 & WPF_RESTORETOMAXIMIZED.0 != 0;
        let p = crate::winplace::Placement { left: r.left, top: r.top, width: r.right - r.left, height: r.bottom - r.top, maximized };
        if p.width >= crate::winplace::MIN_W && p.height >= crate::winplace::MIN_H {
            app::remember_window(p);
        }
    }
}

pub fn create_accelerators() -> windows::core::Result<HACCEL> {
    let k = |key: VIRTUAL_KEY, flags: ACCEL_VIRT_FLAGS, cmd: usize| ACCEL {
        fVirt: flags | FVIRTKEY,
        key: key.0,
        cmd: cmd as u16,
    };
    let table = [
        k(VK_N, FCONTROL, ID_FILE_NEW),
        k(VK_Q, FCONTROL, ID_APP_EXIT),
        k(VK_W, FCONTROL | FSHIFT, ID_FILE_CLOSE),
        k(VK_D, FCONTROL, ID_FILE_DISCONNECT),
        k(VK_S, FCONTROL, ID_FILE_CONNECT),
        k(VK_J, FCONTROL, ID_CHANNEL_JOIN),
        k(VK_L, FCONTROL, ID_CHANNEL_LIST),
        k(VK_T, FCONTROL, ID_FILE_DCC),
        k(VK_P, FCONTROL, ID_OPT_PREFS),
        k(VIRTUAL_KEY(0x46), FCONTROL, ID_EDIT_FIND),
        k(VIRTUAL_KEY(0x46), FCONTROL | FSHIFT, ID_EDIT_LOGSEARCH),
        k(VIRTUAL_KEY(0x72), ACCEL_VIRT_FLAGS(0), ID_EDIT_FINDNEXT),
        k(VIRTUAL_KEY(0x72), FSHIFT, ID_EDIT_FINDPREV),
        k(VK_F9, ACCEL_VIRT_FLAGS(0), ID_VIEW_SERVERS),
        k(VK_F8, ACCEL_VIRT_FLAGS(0), ID_VIEW_EVENTS),
        // Alt+1 .. Alt+5 recall the saved dock layouts
        k(VIRTUAL_KEY(0x31), FALT, ID_LAYOUT_LOAD),
        k(VIRTUAL_KEY(0x32), FALT, ID_LAYOUT_LOAD + 1),
        k(VIRTUAL_KEY(0x33), FALT, ID_LAYOUT_LOAD + 2),
        k(VIRTUAL_KEY(0x34), FALT, ID_LAYOUT_LOAD + 3),
        k(VIRTUAL_KEY(0x35), FALT, ID_LAYOUT_LOAD + 4),
    ];
    unsafe { CreateAcceleratorTableW(&table) }
}

pub fn mdi_client() -> HWND {
    mdi()
}

unsafe fn build_menu() -> windows::core::Result<HMENU> {
    let bar = CreateMenu()?;
    let add = |m: HMENU, id: usize, text: PCWSTR| {
        let _ = AppendMenuW(m, MF_STRING, id, text);
    };
    let sep = |m: HMENU| {
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
    };
    let pop = |bar: HMENU, m: HMENU, text: PCWSTR| {
        let _ = AppendMenuW(bar, MF_POPUP, m.0 as usize, text);
    };

    let file = CreatePopupMenu()?;
    add(file, ID_FILE_NEW, w!("&New Server\tCtrl+N"));
    add(file, ID_FILE_CONNECT, w!("&Connect...\tCtrl+S"));
    add(file, ID_CHANNEL_JOIN, w!("&Join Channel...\tCtrl+J"));
    add(file, ID_FILE_CLOSE, w!("C&lose Window\tCtrl+Shift+W"));
    sep(file);
    add(file, ID_FILE_DISCONNECT, w!("&Disconnect\tCtrl+D"));
    sep(file);
    add(file, ID_FILE_DCC, w!("DCC T&ransfers...\tCtrl+T"));
    add(file, ID_FILE_IMPORT, w!("&Import from HydraIRC..."));
    sep(file);
    add(file, ID_APP_EXIT, w!("E&xit\tCtrl+Q"));
    pop(bar, file, w!("&File"));

    let edit = CreatePopupMenu()?;
    add(edit, ID_EDIT_CUT, w!("Cu&t\tCtrl+X"));
    add(edit, ID_EDIT_COPY, w!("&Copy\tCtrl+C"));
    add(edit, ID_EDIT_PASTE, w!("&Paste\tCtrl+V"));
    sep(edit);
    sep(edit);
    add(edit, ID_EDIT_FIND, w!("&Find...\tCtrl+F"));
    add(edit, ID_EDIT_FINDNEXT, w!("Find &Next\tF3"));
    add(edit, ID_EDIT_FINDPREV, w!("Find Pre&vious\tShift+F3"));
    add(edit, ID_EDIT_LOGSEARCH, w!("&Search Logs...\tCtrl+Shift+F"));
    sep(edit);
    add(edit, ID_EDIT_CLEAR, w!("C&lear Buffer"));
    pop(bar, edit, w!("&Edit"));

    let view = CreatePopupMenu()?;
    add(view, ID_VIEW_STATUS, w!("Status &Bar"));
    add(view, ID_VIEW_TOOLBAR, w!("&Toolbar"));
    sep(view);
    add(view, ID_VIEW_SERVERS, w!("&Server List\tF9"));
    add(view, ID_VIEW_EVENTS, w!("&Event Log\tF8"));
    let panes_menu = CreatePopupMenu()?;
    for p in layouts::PANES {
        let t = wide(p.title());
        let _ = AppendMenuW(panes_menu, MF_STRING | MF_CHECKED, ID_PANE_BASE + p as usize, PCWSTR(t.as_ptr()));
    }
    let _ = AppendMenuW(view, MF_POPUP, panes_menu.0 as usize, w!("&Panes"));
    let edge_menu = CreatePopupMenu()?;
    for e in layouts::EDGES {
        let t = wide(e.title());
        let _ = AppendMenuW(edge_menu, MF_STRING, ID_DOCK_EDGE + e as usize, PCWSTR(t.as_ptr()));
    }
    let _ = AppendMenuW(view, MF_POPUP, edge_menu.0 as usize, w!("Move All Panes &To"));
    let layouts_menu = CreatePopupMenu()?;
    for slot in 0..layouts::SLOTS {
        let t = wide(&format!("&Load Layout {}\tAlt+{}", slot + 1, slot + 1));
        let _ = AppendMenuW(layouts_menu, MF_STRING, ID_LAYOUT_LOAD + slot, PCWSTR(t.as_ptr()));
    }
    let _ = AppendMenuW(layouts_menu, MF_SEPARATOR, 0, PCWSTR::null());
    for slot in 0..layouts::SLOTS {
        let t = wide(&format!("&Save Layout {}", slot + 1));
        let _ = AppendMenuW(layouts_menu, MF_STRING, ID_LAYOUT_SAVE + slot, PCWSTR(t.as_ptr()));
    }
    let _ = AppendMenuW(view, MF_POPUP, layouts_menu.0 as usize, w!("&Layouts"));
    let _ = CheckMenuItem(view, ID_VIEW_STATUS as u32, (MF_BYCOMMAND | MF_CHECKED).0);
    let _ = CheckMenuItem(view, ID_VIEW_TOOLBAR as u32, (MF_BYCOMMAND | MF_CHECKED).0);
    pop(bar, view, w!("&View"));

    let chan = CreatePopupMenu()?;
    add(chan, ID_CHANNEL_LIST, w!("Channel &List...\tCtrl+L"));
    add(chan, ID_CHANNEL_JOIN, w!("&Join Channel...\tCtrl+J"));
    pop(bar, chan, w!("&Channel"));

    let fav = CreatePopupMenu()?;
    add(fav, ID_FAV_ADD, w!("&Add to Favorites"));
    add(fav, ID_FAV_ORGANIZE, w!("&Organize Favorites..."));
    sep(fav);
    pop(bar, fav, w!("F&avorites"));
    FAV_MENU.store(fav.0 as isize, Ordering::Relaxed);

    let opts = CreatePopupMenu()?;
    add(opts, ID_OPT_PREFS, w!("&Preferences...\tCtrl+P"));
    add(opts, ID_OPT_HIGHLIGHTS, w!("&Highlights..."));
    add(opts, ID_OPT_IGNORES, w!("&Ignore List..."));
    add(opts, ID_OPT_PERFORM, w!("Auto-&Perform..."));
    add(opts, ID_OPT_IDENTITIES, w!("I&dentities..."));
    add(opts, ID_OPT_ALIASES, w!("A&liases..."));
    sep(opts);
    let theme_menu = CreatePopupMenu()?;
    add(theme_menu, ID_OPT_THEME, w!("&Light"));
    add(theme_menu, ID_OPT_THEME + 1, w!("&Dark"));
    add(theme_menu, ID_OPT_THEME + 2, w!("&Follow Windows"));
    let _ = AppendMenuW(opts, MF_POPUP, theme_menu.0 as usize, w!("&Theme"));
    add(opts, ID_OPT_COLORS, w!("Chat &Colors..."));
    add(opts, ID_OPT_CHATFONT, w!("Chat F&ont..."));
    add(opts, ID_OPT_UIFONT, w!("&Interface Font..."));
    sep(opts);
    add(opts, ID_OPT_TYPING_SHOW, w!("Show T&yping Indicators"));
    add(opts, ID_OPT_TYPING_SEND, w!("Send Typing &Notices"));
    add(opts, ID_OPT_TIMESTAMPS, w!("Toggle &Timestamps"));
    add(opts, ID_OPT_USERLIST, w!("Toggle &User List"));
    sep(opts);
    add(opts, ID_OPT_LOGGING, w!("Toggle &Logging"));
    add(opts, ID_OPT_OPENLOG, w!("Open This Window's Lo&g"));
    add(opts, ID_OPT_OPENLOGS, w!("Open Log &Folder"));
    pop(bar, opts, w!("&Options"));

    let win = CreatePopupMenu()?;
    add(win, ID_WIN_CASCADE, w!("&Cascade"));
    add(win, ID_WIN_TILEH, w!("Tile &Horizontally"));
    add(win, ID_WIN_TILEV, w!("Tile &Vertically"));
    add(win, ID_WIN_ARRANGE, w!("&Arrange Icons"));
    add(win, ID_WIN_MINALL, w!("&Minimize All"));
    pop(bar, win, w!("&Window"));
    WINDOW_MENU.store(win.0 as isize, Ordering::Relaxed);

    let help = CreatePopupMenu()?;
    add(help, ID_HELP_ABOUT, w!("&About ChimeraIRC"));
    pop(bar, help, w!("&Help"));

    MENU_BAR.store(bar.0 as isize, Ordering::Relaxed);
    Ok(bar)
}

// ---------------------------------------------------------------- frame

/// Toolbar buttons: (command, index into toolbar.bmp, toggle?, tooltip). `None` is a separator.
/// The bitmap is HydraIRC's own (18 icons, 16px, magenta = transparent); only buttons whose
/// features exist in ChimeraIRC are present.
const TOOLBAR_BUTTONS: &[Option<(usize, i32, bool, &str)>] = &[
    Some((ID_FILE_NEW, 0, false, "New Server")),
    Some((ID_FILE_CONNECT, 1, false, "Connect")),
    Some((ID_FILE_DISCONNECT, 2, false, "Disconnect")),
    None,
    Some((ID_WIN_CASCADE, 3, false, "Cascade Windows")),
    Some((ID_WIN_TILEV, 4, false, "Tile Windows Vertically")),
    Some((ID_WIN_TILEH, 5, false, "Tile Windows Horizontally")),
    Some((ID_WIN_MINALL, 6, false, "Minimize All Windows")),
    None,
    Some((ID_OPT_USERLIST, 7, true, "Show or Hide the User List")),
    Some((ID_OPT_LOGGING, 9, true, "Toggle Logging")),
    None,
    Some((ID_FILE_DCC, 15, false, "DCC Transfers")),
];

fn toolbar_tip(id: usize) -> &'static str {
    TOOLBAR_BUTTONS.iter().flatten().find(|b| b.0 == id).map_or("", |b| b.3)
}

/// Load HydraIRC's toolbar bitmap into an image list, scaling by whole factors at high DPI
/// (nearest-neighbour keeps the pixel art crisp and the magenta key exact).
unsafe fn load_icons(res: u16, icons: i32) -> HIMAGELIST {
    let magenta = windows::Win32::Foundation::COLORREF(0x00ff_00ff);
    let Ok(src) = LoadImageW(hinst(), PCWSTR(res as usize as *const u16), IMAGE_BITMAP, 0, 0, LR_CREATEDIBSECTION) else {
        return HIMAGELIST::default();
    };
    let src = HBITMAP(src.0);
    let factor = ((GetDpiForSystem() as i32 + 48) / 96).max(1);
    let size = 16 * factor;
    let himl = ImageList_Create(size, size, ILC_COLOR24 | ILC_MASK, icons, 0);
    if factor == 1 {
        ImageList_AddMasked(himl, src, magenta);
    } else {
        let screen = GetDC(None);
        let (src_dc, dst_dc) = (CreateCompatibleDC(screen), CreateCompatibleDC(screen));
        let dst = CreateCompatibleBitmap(screen, icons * size, size);
        let (old_src, old_dst) = (SelectObject(src_dc, src), SelectObject(dst_dc, dst));
        SetStretchBltMode(dst_dc, COLORONCOLOR);
        let _ = StretchBlt(dst_dc, 0, 0, icons * size, size, src_dc, 0, 0, icons * 16, 16, SRCCOPY);
        SelectObject(src_dc, old_src);
        SelectObject(dst_dc, old_dst);
        ImageList_AddMasked(himl, dst, magenta);
        let _ = DeleteObject(dst);
        let _ = DeleteDC(src_dc);
        let _ = DeleteDC(dst_dc);
        ReleaseDC(None, screen);
    }
    let _ = DeleteObject(src);
    himl
}

/// The toolbar icons drawn for a theme (light strokes for the dark theme, dark strokes for the light one; bright
/// accents, with an alpha channel), at the size that fits the screen's scale: 16, 32 or 48 pixels.
unsafe fn load_themed_icons(dark: bool) -> HIMAGELIST {
    let factor = ((GetDpiForSystem() as i32 + 48) / 96).max(1);
    let base: u16 = if dark { 110 } else { 113 };
    let (res, size) = match factor {
        1 => (base, 16),
        2 => (base + 1, 32),
        _ => (base + 2, 48),
    };
    let Ok(src) = LoadImageW(hinst(), PCWSTR(res as usize as *const u16), IMAGE_BITMAP, 0, 0, LR_CREATEDIBSECTION) else {
        return HIMAGELIST::default();
    };
    let src = HBITMAP(src.0);
    let himl = ImageList_Create(size, size, ILC_COLOR32, 18, 0);
    ImageList_Add(himl, src, HBITMAP::default());
    let _ = DeleteObject(src);
    himl
}

/// The toolbar icons for the current theme (HydraIRC's own bitmap is only the fallback).
unsafe fn toolbar_icons() -> HIMAGELIST {
    let himl = load_themed_icons(crate::look::theme().dark);
    if !himl.is_invalid() {
        return himl;
    }
    load_icons(100, 18)
}

unsafe fn create_toolbar(parent: HWND) {
    let tb = CreateWindowExW(
        WINDOW_EX_STYLE(0), w!("ToolbarWindow32"), PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS
            | WINDOW_STYLE(TBSTYLE_FLAT | TBSTYLE_TRANSPARENT | TBSTYLE_TOOLTIPS | CCS_NODIVIDER as u32 | CCS_NOPARENTALIGN as u32 | CCS_NORESIZE as u32),
        0, 0, 0, 0, parent, None, hinst(), None,
    )
    .unwrap_or_default();
    TOOLBAR.store(tb.0 as isize, Ordering::Relaxed);
    send(tb, TB_BUTTONSTRUCTSIZE, std::mem::size_of::<TBBUTTON>(), 0);
    let himl = toolbar_icons();
    send(tb, TB_SETIMAGELIST, 0, himl.0 as isize);
    let buttons: Vec<TBBUTTON> = TOOLBAR_BUTTONS
        .iter()
        .map(|b| match b {
            Some((id, img, toggle, _)) => TBBUTTON {
                iBitmap: *img,
                idCommand: *id as i32,
                fsState: TBSTATE_ENABLED as u8,
                fsStyle: (if *toggle { BTNS_CHECK } else { BTNS_BUTTON }) as u8,
                ..Default::default()
            },
            None => TBBUTTON { fsStyle: BTNS_SEP as u8, iBitmap: px(8), ..Default::default() },
        })
        .collect();
    send(tb, TB_ADDBUTTONS, buttons.len(), buttons.as_ptr() as isize);
    send(tb, TB_AUTOSIZE, 0, 0);
}

fn toolbar() -> HWND {
    hw(TOOLBAR.load(Ordering::Relaxed))
}

unsafe fn create_frame_children(hwnd: HWND) {
    create_toolbar(hwnd);
    let hi = hinst();
    let tab = CreateWindowExW(
        WINDOW_EX_STYLE(0), w!("SysTabControl32"), PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WINDOW_STYLE(TCS_FOCUSNEVER | TCS_OWNERDRAWFIXED | TCS_FIXEDWIDTH),
        0, 0, 0, 0, hwnd, None, hi, None,
    )
    .unwrap_or_default();
    // Owner-drawn tabs are fixed size; long names get an ellipsis.
    send(tab, TCM_SETITEMSIZE, 0, ((px(22) as isize) << 16) | (px(130) as isize & 0xffff));
    send(tab, WM_SETFONT, ui_font().0, 1);
    TAB.store(tab.0 as isize, Ordering::Relaxed);

    let st = CreateWindowExW(
        WINDOW_EX_STYLE(0), w!("msctls_statusbar32"), PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SBARS_SIZEGRIP),
        0, 0, 0, 0, hwnd, None, hi, None,
    )
    .unwrap_or_default();
    send(st, WM_SETFONT, ui_font().0, 1);
    STATUS.store(st.0 as isize, Ordering::Relaxed);

    let ccs = CLIENTCREATESTRUCT {
        hWindowMenu: HANDLE(WINDOW_MENU.load(Ordering::Relaxed) as *mut c_void),
        idFirstChild: FIRST_CHILD,
    };
    let mdi = CreateWindowExW(
        WS_EX_CLIENTEDGE, w!("MDICLIENT"), PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS | WS_VSCROLL | WS_HSCROLL,
        0, 0, 0, 0, hwnd, None, hi, Some(&ccs as *const _ as *const c_void),
    )
    .unwrap_or_default();
    MDI.store(mdi.0 as isize, Ordering::Relaxed);
    dock::create(hwnd);
}

unsafe fn layout_frame(hwnd: HWND) {
    if mdi().0.is_null() {
        return;
    }
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right, rc.bottom);
    send(status(), WM_SIZE, 0, 0);
    let mut sr = RECT::default();
    let _ = GetWindowRect(status(), &mut sr);
    let sh = if IsWindowVisible(status()).as_bool() { sr.bottom - sr.top } else { 0 };
    let tbh = if SHOW_TOOLBAR.load(Ordering::Relaxed) {
        let mut tr = RECT::default();
        let _ = GetWindowRect(toolbar(), &mut tr);
        let tbh = (tr.bottom - tr.top).max(px(24));
        let _ = MoveWindow(toolbar(), 0, 0, w, tbh, true);
        tbh
    } else {
        0
    };
    let th = px(26);
    // Docks on the top and bottom span the whole width; the left and right ones fill the height between them.
    let mut area = (0, tbh, w, (h - tbh - sh).max(0));
    for e in [layouts::Edge::Top, layouts::Edge::Bottom, layouts::Edge::Left, layouts::Edge::Right] {
        let keep = if e.horizontal() { px(140) } else { px(100) };
        let ((dx, dy, dw, dh), rest) = layouts::split(area, e, dock::size_px(e), keep);
        if !dock::handle(e).0.is_null() {
            dock::place_dock(e, (dx, dy, dw, dh));
        }
        area = rest;
    }
    let (rx, ry, rw, rh) = area;
    let _ = MoveWindow(tab(), rx, ry, rw, th, true);
    let _ = MoveWindow(mdi(), rx, ry + th, rw, (rh - th).max(0), true);
    let parts = [(w - px(360)).max(px(100)), (w - px(200)).max(px(200)), -1i32];
    send(status(), SB_SETPARTS, 3, parts.as_ptr() as isize);
}

thread_local! {
    /// The text of the three parts of the status bar (it is drawn by us, in the theme's colours).
    static STATUS_TEXT: RefCell<[String; 3]> = const { RefCell::new([const { String::new() }; 3]) };
}

/// The theme (or the chat colours) changed: everything that is drawn by us follows.
pub fn theme_changed() {
    unsafe {
        let frame = frame();
        let bar = HMENU(MENU_BAR.load(Ordering::Relaxed) as *mut c_void);
        let current = crate::look::choice();
        let which = [crate::theme::Choice::Light, crate::theme::Choice::Dark, crate::theme::Choice::System].iter().position(|c| *c == current).unwrap_or(2);
        let _ = CheckMenuRadioItem(bar, ID_OPT_THEME as u32, (ID_OPT_THEME + 2) as u32, (ID_OPT_THEME + which) as u32, MF_BYCOMMAND.0);
        crate::look::style_title_bar(frame);
        let _ = DrawMenuBar(frame);
        repaint_tabs();
        // the toolbar icons that suit the theme
        if !toolbar().0.is_null() {
            let old = send(toolbar(), TB_SETIMAGELIST, 0, toolbar_icons().0 as isize);
            if old != 0 {
                let _ = ImageList_Destroy(HIMAGELIST(old));
            }
        }
        for part in 0..3 {
            send_status_part(part);
        }
        let _ = InvalidateRect(status(), None, true);
        let _ = InvalidateRect(toolbar(), None, true);
    }
    reprint_all();
}

/// The fonts changed: make new ones and put them on every control that used the old ones.
pub fn fonts_changed() {
    let (old_ui, old_mono, old_bold) = (UI_FONT.load(Ordering::Relaxed), MONO_FONT.load(Ordering::Relaxed), BOLD_FONT.load(Ordering::Relaxed));
    let ui = crate::look::make_font(&crate::look::ui_font(), false);
    let mono = crate::look::make_font(&crate::look::chat_font(), false);
    let bold = crate::look::make_font(&crate::look::ui_font(), true);
    UI_FONT.store(ui.0 as isize, Ordering::Relaxed);
    MONO_FONT.store(mono.0 as isize, Ordering::Relaxed);
    BOLD_FONT.store(bold.0 as isize, Ordering::Relaxed);
    unsafe {
        let swap = |h: HWND| {
            let f = SendMessageW(h, WM_GETFONT, WPARAM(0), LPARAM(0)).0;
            let new = if f == old_ui { ui.0 as isize } else if f == old_mono { mono.0 as isize } else if f == old_bold { bold.0 as isize } else { 0 };
            if new != 0 {
                SendMessageW(h, WM_SETFONT, WPARAM(new as usize), LPARAM(1));
            }
        };
        extern "system" fn each(h: HWND, lp: LPARAM) -> windows::Win32::Foundation::BOOL {
            let swap = unsafe { &*(lp.0 as *const &dyn Fn(HWND)) };
            swap(h);
            let _ = unsafe { EnumChildWindows(h, Some(each), lp) };
            true.into()
        }
        let dynswap: &dyn Fn(HWND) = &swap;
        let _ = EnumThreadWindows(windows::Win32::System::Threading::GetCurrentThreadId(), Some(each), LPARAM(&dynswap as *const &dyn Fn(HWND) as isize));
        for old in [old_ui, old_mono, old_bold] {
            if old != 0 {
                let _ = DeleteObject(HGDIOBJ(old as *mut c_void));
            }
        }
        // the tab bar and the layout depend on the size of the text
        send(tab(), TCM_SETITEMSIZE, 0, ((px(22) as isize) << 16) | (px(130) as isize & 0xffff));
        relayout();
    }
    reprint_all();
}

pub fn set_status(part: usize, text: &str) {
    if part < 3 {
        STATUS_TEXT.with(|s| s.borrow_mut()[part] = text.to_owned());
    }
    // SBT_OWNERDRAW: the text is not given to the control, WM_DRAWITEM asks for it
    send_status_part(part);
    unsafe {
        let _ = InvalidateRect(status(), None, true);
    }
}

/// Tell the status bar a part is owner-drawn; in the dark theme without the white sunken border.
fn send_status_part(part: usize) {
    let no_border = 0x100; // SBT_NOBORDERS
    send(status(), SB_SETTEXTW, part | 0x1000 | no_border, 0);
}

/// Draw one part of the status bar.
unsafe fn draw_status_part(dis: &DRAWITEMSTRUCT) {
    let t = crate::look::theme();
    let part = dis.itemID as usize;
    let brush = crate::look::panel_brush();
    FillRect(dis.hDC, &dis.rcItem, brush);
    let text = STATUS_TEXT.with(|s| s.borrow().get(part).cloned().unwrap_or_default());
    let old = SelectObject(dis.hDC, HGDIOBJ(UI_FONT.load(Ordering::Relaxed) as *mut c_void));
    SetBkMode(dis.hDC, TRANSPARENT);
    SetTextColor(dis.hDC, crate::look::colorref(t.panel_text));
    let mut u: Vec<u16> = text.encode_utf16().collect();
    let mut rc = RECT { left: dis.rcItem.left + px(4), ..dis.rcItem };
    // (drawing with an empty text or an empty rectangle is left alone: Windows can fault in it)
    if !u.is_empty() && rc.right - rc.left > px(8) && rc.bottom > rc.top {
        u.push(0);
        let n = u.len() - 1;
        DrawTextW(dis.hDC, &mut u[..n], &mut rc, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
    }
    SelectObject(dis.hDC, old);
}

/// Repaint the tab strip (activity colours changed).
fn repaint_tabs() {
    unsafe {
        let _ = InvalidateRect(tab(), None, false);
    }
}

/// Owner-draw one tab: window-type icon (HydraIRC's tab icons) plus the name, coloured by activity.
unsafe fn draw_tab(dis: &DRAWITEMSTRUCT) {
    let hdc = dis.hDC;
    let rc = dis.rcItem;
    let selected = dis.itemState.0 & ODS_SELECTED.0 != 0;
    let mut item = TCITEMW { mask: TCIF_PARAM, ..Default::default() };
    send(tab(), TCM_GETITEMW, dis.itemID as usize, &mut item as *mut _ as isize);
    let (kind, name, act) = child(hw(item.lParam.0))
        .map(|c| (c.kind, c.name.clone(), c.activity))
        .unwrap_or((Kind::Server, String::new(), Activity::None));

    let t = crate::look::theme();
    let cr = |c: Rgb| crate::look::colorref(c).0;
    let (bg, fg, bold) = if selected {
        (cr(t.accent), cr(t.accent_text), false)
    } else {
        // the activity colours are made readable on the panel (or on the highlight's own background)
        let on_panel = |c: Rgb| cr(crate::theme::readable_on(c, t.panel));
        let highlight_bg = if t.dark { Rgb(0x5a, 0x4a, 0x10) } else { Rgb(0xff, 0xf4, 0xb8) };
        match act {
            Activity::None => (cr(t.panel), cr(t.panel_text), false),
            Activity::Events => (cr(t.panel), on_panel(Rgb(0x00, 0x50, 0xc0)), false),
            Activity::Messages => (cr(t.panel), on_panel(Rgb(0xc0, 0x00, 0x00)), false),
            Activity::Highlight => (cr(highlight_bg), cr(crate::theme::readable_on(Rgb(0xc0, 0x00, 0x00), highlight_bg)), true),
        }
    };
    let brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(bg));
    FillRect(hdc, &rc, brush);
    let _ = DeleteObject(brush);

    let himl = HIMAGELIST(TAB_ICONS.load(Ordering::Relaxed));
    let (mut cx, mut cy) = (16, 16);
    let _ = ImageList_GetIconSize(himl, Some(&mut cx), Some(&mut cy));
    let icon = match kind {
        Kind::Server => 0,
        Kind::Channel => 2,
        Kind::Query | Kind::DccChat => 3,
    };
    let pad = px(4);
    let _ = ImageList_Draw(himl, icon, hdc, rc.left + pad, rc.top + (rc.bottom - rc.top - cy) / 2, ILD_TRANSPARENT);

    let font = if bold { BOLD_FONT.load(Ordering::Relaxed) } else { UI_FONT.load(Ordering::Relaxed) };
    let old = SelectObject(hdc, HGDIOBJ(font as *mut c_void));
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(fg));
    let mut text: Vec<u16> = name.encode_utf16().collect();
    let mut tr = RECT { left: rc.left + pad * 2 + cx, top: rc.top, right: rc.right - pad, bottom: rc.bottom };
    if !text.is_empty() && tr.right - tr.left > px(8) && tr.bottom > tr.top {
        text.push(0);
        let n = text.len() - 1;
        DrawTextW(hdc, &mut text[..n], &mut tr, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
    }
    SelectObject(hdc, old);
}

fn tab_index(child: HWND) -> Option<usize> {
    let n = send(tab(), TCM_GETITEMCOUNT, 0, 0) as usize;
    for i in 0..n {
        let mut item = TCITEMW { mask: TCIF_PARAM, ..Default::default() };
        send(tab(), TCM_GETITEMW, i, &mut item as *mut _ as isize);
        if item.lParam.0 == raw(child) {
            return Some(i);
        }
    }
    None
}

fn tab_add(child: HWND, title: &str) {
    let mut t = wide(title);
    let item = TCITEMW {
        mask: TCIF_TEXT | TCIF_PARAM,
        pszText: PWSTR(t.as_mut_ptr()),
        lParam: LPARAM(raw(child)),
        ..Default::default()
    };
    let n = send(tab(), TCM_GETITEMCOUNT, 0, 0) as usize;
    send(tab(), TCM_INSERTITEMW, n, &item as *const _ as isize);
}

fn tab_remove(child: HWND) {
    if let Some(i) = tab_index(child) {
        send(tab(), TCM_DELETEITEM, i, 0);
    }
}

fn tab_select(child: HWND) {
    if let Some(i) = tab_index(child) {
        send(tab(), TCM_SETCURSEL, i, 0);
    }
}

/// `NMCUSTOMDRAW`, the part of it that is used.
#[repr(C)]
struct NmCustomDraw {
    hdr: NMHDR,
    stage: u32,
    hdc: HDC,
    rc: RECT,
    item: usize,
    state: u32,
    lparam: isize,
}

/// Custom drawing of the toolbar: a button under the pointer, pressed, or checked gets a clearly visible fill and a
/// thin border (the system's own hot look is nearly invisible in the light theme).
unsafe fn toolbar_custom_draw(d: &NmCustomDraw) -> isize {
    const CDDS_PREPAINT: u32 = 0x1;
    const CDDS_ITEMPREPAINT: u32 = 0x10001;
    const CDRF_NOTIFYITEMDRAW: isize = 0x20;
    const TBCDRF_NOBACKGROUND: isize = 0x0040_0000;
    const CDIS_SELECTED: u32 = 0x1;
    const CDIS_DISABLED: u32 = 0x4;
    const CDIS_CHECKED: u32 = 0x8;
    const CDIS_HOT: u32 = 0x40;
    if d.stage == CDDS_PREPAINT {
        return CDRF_NOTIFYITEMDRAW;
    }
    if d.stage != CDDS_ITEMPREPAINT || d.state & CDIS_DISABLED != 0 {
        return 0;
    }
    let dark = crate::look::theme().dark;
    // (fill, border)
    let (fill, border) = if d.state & CDIS_SELECTED != 0 {
        if dark { ((0x25, 0x3e, 0x64), (0x4f, 0x80, 0xc4)) } else { ((0xa4, 0xbc, 0xdd), (0x5f, 0x84, 0xbb)) }
    } else if d.state & CDIS_CHECKED != 0 {
        if dark { ((0x2d, 0x4c, 0x7a), (0x4f, 0x80, 0xc4)) } else { ((0xb9, 0xcd, 0xe8), (0x6a, 0x8f, 0xc4)) }
    } else if d.state & CDIS_HOT != 0 {
        if dark { ((0x3c, 0x44, 0x52), (0x5a, 0x66, 0x7c)) } else { ((0xcf, 0xdc, 0xee), (0x8f, 0xab, 0xd3)) }
    } else {
        return 0;
    };
    let rgb = |c: (u8, u8, u8)| windows::Win32::Foundation::COLORREF(c.0 as u32 | (c.1 as u32) << 8 | (c.2 as u32) << 16);
    let (fb, bb) = (CreateSolidBrush(rgb(fill)), CreateSolidBrush(rgb(border)));
    FillRect(d.hdc, &d.rc, fb);
    FrameRect(d.hdc, &d.rc, bb);
    let _ = DeleteObject(HGDIOBJ(fb.0));
    let _ = DeleteObject(HGDIOBJ(bb.0));
    TBCDRF_NOBACKGROUND
}

/// The menu of a tab in the main tab strip: the same commands as the File menu, with their shortcuts.
unsafe fn tab_context_menu(frame_hwnd: HWND) {
    let mut pt = windows::Win32::Foundation::POINT::default();
    let _ = GetCursorPos(&mut pt);
    let mut local = pt;
    let _ = ScreenToClient(tab(), &mut local);
    let mut hit = TCHITTESTINFO { pt: local, flags: Default::default() };
    let i = send(tab(), TCM_HITTEST, 0, &mut hit as *mut _ as isize);
    if i < 0 {
        return;
    }
    let mut item = TCITEMW { mask: TCIF_PARAM, ..Default::default() };
    send(tab(), TCM_GETITEMW, i as usize, &mut item as *mut _ as isize);
    let target = hw(item.lParam.0);
    let Some(c) = child(target) else { return };
    let is_server = c.kind == Kind::Server;
    let conn = c.conn;
    let Ok(menu) = CreatePopupMenu() else { return };
    let _ = AppendMenuW(menu, MF_STRING, 1, w!("C&lose Window\tCtrl+Shift+W"));
    let _ = AppendMenuW(menu, MF_STRING, 2, w!("Close &Other Windows"));
    if is_server {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 3, w!("&Disconnect\tCtrl+D"));
    }
    let _ = SetForegroundWindow(frame_hwnd);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, frame_hwnd, None).0;
    let _ = DestroyMenu(menu);
    match cmd {
        1 => {
            let _ = PostMessageW(target, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        2 => {
            let mut all: Vec<HWND> = Vec::new();
            let _ = EnumChildWindows(mdi(), Some(collect_children), LPARAM(&mut all as *mut Vec<HWND> as isize));
            for h in all.into_iter().filter(|h| *h != target) {
                let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        3 => app::disconnect(conn),
        _ => {}
    }
}

fn forward_to_focus(msg: u32) {
    let f = unsafe { GetFocus() };
    if !f.0.is_null() {
        send(f, msg, 0, 0);
    }
}

/// EnumChildWindows callback: collects our direct MDI children (not their controls).
unsafe extern "system" fn collect_children(h: HWND, lp: LPARAM) -> windows::Win32::Foundation::BOOL {
    if GetParent(h).is_ok_and(|p| p == mdi()) {
        (*(lp.0 as *mut Vec<HWND>)).push(h);
    }
    true.into()
}

/// Apply persisted/toggled view state to the UI and the menu/toolbar checks.
/// Apply the saved dock layout (shown, width in 96-dpi pixels, pane in front) and lay the window out.
pub fn apply_dock(l: Layout) {
    let l = l.normalized();
    dock::apply(l);
    unsafe {
        let bar = HMENU(MENU_BAR.load(Ordering::Relaxed) as *mut c_void);
        let check = |id: usize, on: bool| {
            let flag = if on { MF_CHECKED } else { MF_UNCHECKED };
            let _ = CheckMenuItem(bar, id as u32, (MF_BYCOMMAND | flag).0);
        };
        check(ID_VIEW_SERVERS, l.in_front(layouts::Pane::Servers));
        check(ID_VIEW_EVENTS, l.in_front(layouts::Pane::Events));
        for p in layouts::PANES {
            check(ID_PANE_BASE + p as usize, l.shows(p));
        }
        match l.single_edge() {
            Some(e) => {
                let _ = CheckMenuRadioItem(bar, ID_DOCK_EDGE as u32, (ID_DOCK_EDGE + 3) as u32, (ID_DOCK_EDGE + e as usize) as u32, MF_BYCOMMAND.0);
            }
            // several docks: none of "everything on one side" is true
            None => {
                for e in layouts::EDGES {
                    check(ID_DOCK_EDGE + e as usize, false);
                }
            }
        }
    }
    relayout();
}

/// Lay out the frame and every child window again (after a size change of the dock, say).
pub fn relayout() {
    unsafe {
        let frame = frame();
        if frame.0.is_null() {
            return;
        }
        layout_frame(frame);
        let mut kids: Vec<HWND> = Vec::new();
        if !mdi().0.is_null() {
            let _ = EnumChildWindows(mdi(), Some(collect_children), LPARAM(&mut kids as *mut _ as isize));
        }
        for k in kids {
            layout_child(k);
        }
    }
}

pub fn apply_view(show_toolbar: bool, show_userlist: bool, logging: bool) {
    SHOW_TOOLBAR.store(show_toolbar, Ordering::Relaxed);
    SHOW_USERLIST.store(show_userlist, Ordering::Relaxed);
    LOGGING.store(logging, Ordering::Relaxed);
    unsafe {
        let bar = HMENU(MENU_BAR.load(Ordering::Relaxed) as *mut c_void);
        let check = |id: usize, on: bool| {
            let flag = if on { MF_CHECKED } else { MF_UNCHECKED };
            let _ = CheckMenuItem(bar, id as u32, (MF_BYCOMMAND | flag).0);
        };
        check(ID_VIEW_TOOLBAR, show_toolbar);
        check(ID_OPT_USERLIST, show_userlist);
        check(ID_OPT_LOGGING, logging);
        if !toolbar().0.is_null() {
            send(toolbar(), TB_CHECKBUTTON, ID_OPT_LOGGING, logging as isize);
            let _ = ShowWindow(toolbar(), if show_toolbar { SW_SHOW } else { SW_HIDE });
            send(toolbar(), TB_CHECKBUTTON, ID_OPT_USERLIST, show_userlist as isize);
        }
        let frame = frame();
        if !frame.0.is_null() {
            layout_frame(frame);
        }
        let mut kids: Vec<HWND> = Vec::new();
        if !mdi().0.is_null() {
            let _ = EnumChildWindows(mdi(), Some(collect_children), LPARAM(&mut kids as *mut _ as isize));
        }
        for k in kids {
            layout_child(k);
        }
    }
}

unsafe fn on_command(hwnd: HWND, id: usize) -> bool {
    if let Some((server, channel)) = favorite_for_command(id) {
        app::favorite_action(server, channel);
        return true;
    }
    match id {
        ID_FILE_IMPORT => app::import_hydrairc_command(),
        ID_FILE_DCC => app::open_transfers_window(),
        ID_FAV_ADD => app::add_to_favorites(),
        ID_FAV_ORGANIZE => app::organize_favorites(),
        ID_FILE_NEW => {
            app::new_server_window();
        }
        ID_FILE_CONNECT => app::connect_dialog(),
        ID_CHANNEL_JOIN => app::join_dialog(),
        ID_CHANNEL_LIST => app::channel_list_command(),
        ID_OPT_PREFS => app::prefs_dialog(),
        ID_OPT_HIGHLIGHTS => app::highlights_dialog(),
        ID_OPT_IGNORES => app::ignore_dialog(),
        ID_OPT_PERFORM => app::perform_dialog(None),
        ID_OPT_IDENTITIES => app::identity_dialog(None),
        ID_OPT_ALIASES => app::alias_dialog(),
        ID_EDIT_FIND => app::find_dialog(),
        ID_EDIT_FINDNEXT => app::find_again(true),
        ID_EDIT_FINDPREV => app::find_again(false),
        ID_EDIT_LOGSEARCH => app::log_search_dialog(),
        i if (ID_OPT_THEME..ID_OPT_THEME + 3).contains(&i) => {
            app::set_theme([crate::theme::Choice::Light, crate::theme::Choice::Dark, crate::theme::Choice::System][i - ID_OPT_THEME])
        }
        ID_OPT_COLORS => app::colors_dialog(),
        ID_OPT_CHATFONT => app::font_dialog(true),
        ID_OPT_UIFONT => app::font_dialog(false),
        ID_OPT_LOGGING => app::set_logging(!LOGGING.load(Ordering::Relaxed)),
        ID_OPT_OPENLOGS => app::open_log_folder(),
        ID_OPT_OPENLOG => app::open_window_log(),
        ID_OPT_TIMESTAMPS => app::toggle_timestamps(),
        ID_OPT_TYPING_SHOW => app::toggle_typing_show(),
        ID_OPT_TYPING_SEND => app::toggle_typing_send(),
        ID_FILE_CLOSE => {
            if let Some(c) = active_child() {
                let _ = PostMessageW(c, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        ID_FILE_DISCONNECT => {
            if let Some(c) = active_child().and_then(|h| child(h).map(|c| c.conn)) {
                app::disconnect(c);
            }
        }
        ID_APP_EXIT => {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        ID_EDIT_CUT => forward_to_focus(WM_CUT),
        ID_EDIT_COPY => forward_to_focus(WM_COPY),
        ID_EDIT_PASTE => forward_to_focus(WM_PASTE),
        ID_EDIT_CLEAR => {
            if let Some(c) = active_child().and_then(|h| child(h)) {
                let _ = SetWindowTextW(c.output, w!(""));
            }
        }
        ID_VIEW_STATUS => {
            let vis = IsWindowVisible(status()).as_bool();
            let _ = ShowWindow(status(), if vis { SW_HIDE } else { SW_SHOW });
            let bar = HMENU(MENU_BAR.load(Ordering::Relaxed) as *mut c_void);
            let flag = if vis { MF_UNCHECKED } else { MF_CHECKED };
            let _ = CheckMenuItem(bar, ID_VIEW_STATUS as u32, (MF_BYCOMMAND | flag).0);
            layout_frame(hwnd);
        }
        ID_VIEW_TOOLBAR => {
            let show = !SHOW_TOOLBAR.load(Ordering::Relaxed);
            app::set_show_toolbar(show);
        }
        ID_VIEW_SERVERS => app::dock_toggle(layouts::Pane::Servers as u8),
        ID_VIEW_EVENTS => app::dock_toggle(layouts::Pane::Events as u8),
        i if (ID_PANE_BASE..ID_PANE_BASE + layouts::PANES.len()).contains(&i) => app::dock_pane_switch(i - ID_PANE_BASE),
        i if (ID_DOCK_EDGE..ID_DOCK_EDGE + 4).contains(&i) => app::dock_set_edge(i - ID_DOCK_EDGE),
        i if (ID_LAYOUT_LOAD..ID_LAYOUT_LOAD + layouts::SLOTS).contains(&i) => app::layout_load(i - ID_LAYOUT_LOAD),
        i if (ID_LAYOUT_SAVE..ID_LAYOUT_SAVE + layouts::SLOTS).contains(&i) => app::layout_save(i - ID_LAYOUT_SAVE),
        ID_OPT_USERLIST => {
            let show = !SHOW_USERLIST.load(Ordering::Relaxed);
            app::set_show_userlist(show);
        }
        ID_WIN_MINALL => {
            let mut kids: Vec<HWND> = Vec::new();
            let _ = EnumChildWindows(mdi(), Some(collect_children), LPARAM(&mut kids as *mut _ as isize));
            for k in kids {
                let _ = ShowWindow(k, SW_MINIMIZE);
            }
        }
        ID_WIN_CASCADE => {
            send(mdi(), WM_MDICASCADE, 0, 0);
        }
        ID_WIN_TILEH => {
            send(mdi(), WM_MDITILE, MDITILE_HORIZONTAL.0 as usize, 0);
        }
        ID_WIN_TILEV => {
            send(mdi(), WM_MDITILE, MDITILE_VERTICAL.0 as usize, 0);
        }
        ID_WIN_ARRANGE => {
            send(mdi(), WM_MDIICONARRANGE, 0, 0);
        }
        ID_HELP_ABOUT => crate::about::show(hwnd),
        _ => return false,
    }
    true
}

extern "system" fn frame_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                FRAME.store(hwnd.0 as isize, Ordering::Relaxed);
                create_frame_children(hwnd);
                SetTimer(hwnd, TYPING_TIMER, 1000, None);
                SetTimer(hwnd, PASTE_TIMER, 700, None);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == PASTE_TIMER => {
                app::paste_tick();
                LRESULT(0)
            }
            WM_TIMER if wp.0 == TYPING_TIMER => {
                app::typing_tick();
                LRESULT(0)
            }
            WM_SIZE => {
                layout_frame(hwnd);
                LRESULT(0)
            }
            WM_COMMAND => {
                // lp == 0: menu or accelerator; lp == toolbar: a toolbar button.
                if (lp.0 == 0 || lp.0 == raw(toolbar())) && on_command(hwnd, wp.0 & 0xffff) {
                    return LRESULT(0);
                }
                DefFrameProcW(hwnd, mdi(), msg, wp, lp)
            }
            WM_NOTIFY => {
                let nm = &*(lp.0 as *const NMHDR);
                if nm.hwndFrom == tab() && nm.code as i32 == TCN_SELCHANGE as i32 {
                    let i = send(tab(), TCM_GETCURSEL, 0, 0);
                    if i >= 0 {
                        let mut item = TCITEMW { mask: TCIF_PARAM, ..Default::default() };
                        send(tab(), TCM_GETITEMW, i as usize, &mut item as *mut _ as isize);
                        let target = hw(item.lParam.0);
                        if IsIconic(target).as_bool() {
                            send(mdi(), WM_MDIRESTORE, raw(target) as usize, 0);
                        }
                        send(mdi(), WM_MDIACTIVATE, raw(target) as usize, 0);
                    }
                    return LRESULT(0);
                }
                // toolbar buttons: the hot, pressed and checked looks are drawn here, in colours that show
                if nm.hwndFrom == toolbar() && nm.code == NM_CUSTOMDRAW {
                    return LRESULT(toolbar_custom_draw(&*(lp.0 as *const NmCustomDraw)));
                }
                // right-click on a tab: the menu for that window
                if nm.hwndFrom == tab() && nm.code == NM_RCLICK {
                    tab_context_menu(hwnd);
                    return LRESULT(1);
                }
                if nm.code == TTN_GETDISPINFOW {
                    let info = &mut *(lp.0 as *mut NMTTDISPINFOW);
                    let tip: Vec<u16> = toolbar_tip(nm.idFrom).encode_utf16().take(79).collect();
                    info.szText[..tip.len()].copy_from_slice(&tip);
                    info.szText[tip.len()] = 0;
                    info.lpszText = windows::core::PWSTR(info.szText.as_mut_ptr());
                    return LRESULT(0);
                }
                DefFrameProcW(hwnd, mdi(), msg, wp, lp)
            }
            WM_DRAWITEM => {
                let dis = &*(lp.0 as *const DRAWITEMSTRUCT);
                if dis.hwndItem == tab() {
                    draw_tab(dis);
                    return LRESULT(1);
                }
                if dis.hwndItem == status() {
                    draw_status_part(dis);
                    return LRESULT(1);
                }
                DefFrameProcW(hwnd, mdi(), msg, wp, lp)
            }
            0x0091 | 0x0092 => match crate::look::menu_bar_message(hwnd, msg, lp) {
                Some(r) => r,
                None => DefFrameProcW(hwnd, mdi(), msg, wp, lp),
            },
            // measuring a menu bar item: the maximized chat window's system-menu icon takes no room
            0x0094 => {
                let r = DefFrameProcW(hwnd, mdi(), msg, wp, lp);
                crate::look::hide_system_icon_width(lp);
                r
            }
            WM_NCPAINT | WM_NCACTIVATE => {
                let r = DefFrameProcW(hwnd, mdi(), msg, wp, lp);
                crate::look::paint_menu_bar_line(hwnd);
                r
            }
            WM_SETFOCUS => {
                if let Some(c) = active_child() {
                    let _ = SetFocus(c);
                }
                LRESULT(0)
            }
            WM_APP_EVENT => {
                app::drain_events();
                LRESULT(0)
            }
            WM_CLOSE => {
                remember_frame(hwnd);
                dock::remember_columns();
                app::shutdown();
                DefFrameProcW(hwnd, mdi(), msg, wp, lp)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefFrameProcW(hwnd, mdi(), msg, wp, lp),
        }
    }
}

// ---------------------------------------------------------------- child windows

pub fn create_child(kind: Kind, conn: u32, name: &str) -> HWND {
    create_child_ex(kind, conn, name, true)
}

/// Create a child window. With `take_focus == false` (e.g. an incoming private message) the
/// previously active window keeps the focus and the new window's tab just lights up.
/// The short tag of a window kind in the remembered-windows key.
fn kind_tag(kind: Kind) -> &'static str {
    match kind {
        Kind::Server => "srv",
        Kind::Channel => "ch",
        Kind::Query => "q",
        Kind::DccChat => "dcc",
    }
}

/// Remember where a chat window is (its restored place, and whether it is maximized).
fn remember_child_place(hwnd: HWND) {
    let Some((kind, name)) = child(hwnd).map(|c| (c.kind, c.name.clone())) else { return };
    unsafe {
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        if GetWindowPlacement(hwnd, &mut wp).is_err() {
            return;
        }
        let r = wp.rcNormalPosition;
        let maximized = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32 || wp.flags.0 & WPF_RESTORETOMAXIMIZED.0 != 0;
        app::remember_child_window(crate::winsizes::Saved {
            key: crate::winsizes::key(kind_tag(kind), &name),
            left: r.left,
            top: r.top,
            width: r.right - r.left,
            height: r.bottom - r.top,
            maximized,
        });
    }
}

pub fn create_child_ex(kind: Kind, conn: u32, name: &str, take_focus: bool) -> HWND {
    let previous = if take_focus { None } else { active_child() };
    let data = Box::into_raw(Box::new(Child {
        kind,
        conn,
        name: name.to_owned(),
        output: HWND::default(),
        input: HWND::default(),
        topic: HWND::default(),
        list: HWND::default(),
        members: Vec::new(),
        away: std::collections::HashSet::new(),
        typing_out: crate::typing::Outgoing::default(),
        typing_in: crate::typing::Incoming::default(),
        dcc: 0,
        activity: Activity::None,
        recent: Vec::new(),
        completion: None,
        history: Vec::new(),
        hist_pos: 0,
        lines: std::collections::VecDeque::new(),
    }));
    let title = wide(name);
    // where this window was when it was last closed (maximized and cascaded like before when unknown)
    let (x, y, w, h, state_style) = match app::saved_child_window(kind_tag(kind), name) {
        Some(s) => {
            let mut rc = RECT::default();
            unsafe {
                let _ = GetClientRect(mdi(), &mut rc);
            }
            let (l, t, w, h) = crate::winsizes::fit(&s, rc.right, rc.bottom);
            (l, t, w, h, if s.maximized { WS_MAXIMIZE } else { WINDOW_STYLE(0) })
        }
        None => (CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, WS_MAXIMIZE),
    };
    let created = unsafe {
        CreateMDIWindowW(
            CHILD_CLASS,
            PCWSTR(title.as_ptr()),
            WS_CLIPCHILDREN | WS_CLIPSIBLINGS | WS_VISIBLE | state_style,
            x, y, w, h,
            mdi(), hinst(), LPARAM(data as isize),
        )
        .unwrap_or_default()
    };
    if let Some(p) = previous {
        activate(p);
    }
    crate::look::style_tree(created);
    created
}

unsafe fn make_edit(parent: HWND, style: WINDOW_STYLE, font: WPARAM) -> HWND {
    let h = CreateWindowExW(
        WS_EX_CLIENTEDGE, w!("EDIT"), PCWSTR::null(), WS_CHILD | WS_VISIBLE | style,
        0, 0, 0, 0, parent, None, hinst(), None,
    )
    .unwrap_or_default();
    send(h, WM_SETFONT, font.0, 1);
    h
}

unsafe fn build_controls(hwnd: HWND, c: &mut Child) {
    c.output = CreateWindowExW(
        WS_EX_CLIENTEDGE, MSFTEDIT_CLASS, PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WS_VSCROLL
            | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | ES_NOHIDESEL) as u32),
        0, 0, 0, 0, hwnd, None, hinst(), None,
    )
    .unwrap_or_default();
    send(c.output, WM_SETFONT, mono_font().0, 1);
    send(c.output, EM_SETBKGNDCOLOR, 0, crate::look::colorref(crate::look::theme().bg).0 as isize);
    send(c.output, EM_SETLIMITTEXT, 0x7fff_fffe, 0);
    send(c.output, EM_AUTOURLDETECT, AURL_ENABLEURL as usize, 0);
    // links (click to open, hover to describe) and the mouse (double click a channel or nick, right click a nick)
    send(c.output, EM_SETEVENTMASK, 0, (ENM_LINK | 0x0002_0000 /* ENM_MOUSEEVENTS */) as isize);
    c.input = make_edit(hwnd, WINDOW_STYLE(ES_AUTOHSCROLL as u32), mono_font());
    let _ = SetWindowSubclass(c.input, Some(input_proc), 1, raw(hwnd) as usize);
    // files can be dropped on a chat window: a private chat sends the file to that person, a channel needs a name
    windows::Win32::UI::Shell::DragAcceptFiles(hwnd, true);
    for ctl in [c.output, c.input] {
        windows::Win32::UI::Shell::DragAcceptFiles(ctl, true);
    }
    let _ = SetWindowSubclass(c.output, Some(drop_proc), 2, raw(hwnd) as usize);
    if c.kind == Kind::Channel {
        c.topic = make_edit(hwnd, WINDOW_STYLE((ES_AUTOHSCROLL | ES_READONLY) as u32), ui_font());
        c.list = CreateWindowExW(
            WS_EX_CLIENTEDGE, w!("LISTBOX"), PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | WINDOW_STYLE((LBS_NOINTEGRALHEIGHT | LBS_NOTIFY) as u32),
            0, 0, 0, 0, hwnd, None, hinst(), None,
        )
        .unwrap_or_default();
        send(c.list, WM_SETFONT, ui_font().0, 1);
        // a file dropped on a name in the list is offered to that person
        windows::Win32::UI::Shell::DragAcceptFiles(c.list, true);
        let _ = SetWindowSubclass(c.list, Some(drop_proc), 2, raw(hwnd) as usize);
    }
}

unsafe fn layout_child(hwnd: HWND) {
    let Some(c) = child(hwnd) else { return };
    if c.output.0.is_null() {
        return;
    }
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right, rc.bottom);
    let in_h = px(24);
    let _ = MoveWindow(c.input, 0, h - in_h, w, in_h, true);
    if c.kind == Kind::Channel {
        let th = px(22);
        let show = SHOW_USERLIST.load(Ordering::Relaxed);
        let lw = if show { px(150).min(w / 2) } else { 0 };
        let _ = ShowWindow(c.list, if show { SW_SHOW } else { SW_HIDE });
        let _ = MoveWindow(c.topic, 0, 0, w, th, true);
        let _ = MoveWindow(c.output, 0, th, w - lw, (h - th - in_h).max(0), true);
        let _ = MoveWindow(c.list, w - lw, th, lw, (h - th - in_h).max(0), true);
    } else {
        let _ = MoveWindow(c.output, 0, 0, w, (h - in_h).max(0), true);
    }
}

extern "system" fn child_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                let cs = &*(lp.0 as *const CREATESTRUCTW);
                let mcs = &*(cs.lpCreateParams as *const MDICREATESTRUCTW);
                let data = mcs.lParam.0 as *mut Child;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, data as isize);
                build_controls(hwnd, &mut *data);
                tab_add(hwnd, &(*data).name);
                LRESULT(0)
            }
            WM_SIZE => {
                layout_child(hwnd);
                DefMDIChildProcW(hwnd, msg, wp, lp)
            }
            WM_MDIACTIVATE => {
                if lp.0 == raw(hwnd) {
                    tab_select(hwnd);
                    if let Some(c) = child(hwnd) {
                        if c.activity != Activity::None {
                            c.activity = Activity::None;
                            repaint_tabs();
                        }
                        let _ = SetFocus(c.input);
                    }
                    app::refresh_status();
                }
                DefMDIChildProcW(hwnd, msg, wp, lp)
            }
            WM_SETFOCUS => {
                if let Some(c) = child(hwnd) {
                    let _ = SetFocus(c.input);
                }
                DefMDIChildProcW(hwnd, msg, wp, lp)
            }
            WM_COMMAND => {
                // Double-click on a user-list row opens a query with that user.
                if let Some(c) = child(hwnd) {
                    // the entry box changed: tell the other end we are typing (or not)
                    if lp.0 == raw(c.input) && (wp.0 >> 16) as u32 == EN_CHANGE {
                        typing_edited(hwnd);
                    }
                    if kind_is_channel(c) && lp.0 == raw(c.list) && (wp.0 >> 16) as u32 == LBN_DBLCLK {
                        let sel = send(c.list, LB_GETCURSEL, 0, 0);
                        if sel >= 0 {
                            if let Some((_, nick)) = c.members.get(sel as usize).cloned() {
                                app::user_action(hwnd, UserAction::Query, nick);
                            }
                        }
                        return LRESULT(0);
                    }
                }
                DefMDIChildProcW(hwnd, msg, wp, lp)
            }
            WM_CONTEXTMENU => {
                if let Some(c) = child(hwnd) {
                    if kind_is_channel(c) && wp.0 == raw(c.list) as usize {
                        let (x, y) = ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32);
                        if let Some((nick, sx, sy)) = user_at(c, x, y) {
                            let me = app::nick_of(c.conn);
                            let op = can_op(c, &me);
                            let ignored = app::is_ignoring_nick(&nick);
                            let buddy = app::is_buddy(&nick);
                            if let Some(action) = user_menu(hwnd, sx, sy, op, ignored, buddy) {
                                app::user_action(hwnd, action, nick);
                            }
                        }
                        return LRESULT(0);
                    }
                }
                DefMDIChildProcW(hwnd, msg, wp, lp)
            }
            WM_NOTIFY => {
                let nm = &*(lp.0 as *const NMHDR);
                if nm.code == EN_LINK {
                    let el = &*(lp.0 as *const ENLINK);
                    if el.msg == WM_LBUTTONUP {
                        open_link(nm.hwndFrom, el.chrg);
                    } else if el.msg == WM_MOUSEMOVE || el.msg == WM_SETCURSOR {
                        // what the address really is, in the status bar while the pointer is over it
                        if let Some(text) = crate::linkinfo::describe(&link_text(nm.hwndFrom, el.chrg)) {
                            LINK_SEEN.store(true, Ordering::Relaxed);
                            HOVER_SHOWING.store(true, Ordering::Relaxed);
                            set_status(0, &text);
                        }
                    }
                    return LRESULT(0);
                }
                if nm.code == EN_MSGFILTER {
                    let mf = &*(lp.0 as *const MsgFilter);
                    if let Some(c) = child(hwnd) {
                        if nm.hwndFrom == c.output {
                            // where the pointer was for this message, in the text window's own coordinates (the
                            // lParam in the notification is not filled in for mouse messages)
                            let at = {
                                let p = GetMessagePos();
                                let mut pt = windows::Win32::Foundation::POINT { x: (p & 0xffff) as i16 as i32, y: ((p >> 16) & 0xffff) as i16 as i32 };
                                let _ = ScreenToClient(c.output, &mut pt);
                                ((pt.y as isize & 0xffff) << 16) | (pt.x as isize & 0xffff)
                            };
                            match mf.msg {
                                WM_MOUSEMOVE => {
                                    let _ = PostMessageW(hwnd, WM_HOVER_CHECK, WPARAM(0), LPARAM(0));
                                }
                                WM_LBUTTONDBLCLK => {
                                    if let Some(word) = word_under(c.output, at) {
                                        app::chat_word_clicked(hwnd, &word);
                                    }
                                }
                                WM_RBUTTONUP if kind_is_channel(c) => {
                                    if let Some(crate::linkinfo::Named::Nick(nick)) = word_under(c.output, at).and_then(|w| crate::linkinfo::classify(&w)) {
                                        if has_member(hwnd, &nick) {
                                            let mut pt = windows::Win32::Foundation::POINT { x: (at & 0xffff) as i16 as i32, y: ((at >> 16) & 0xffff) as i16 as i32 };
                                            let _ = ClientToScreen(c.output, &mut pt);
                                            let me = app::nick_of(c.conn);
                                            let (op, ignored, buddy) = (can_op(c, &me), app::is_ignoring_nick(&nick), app::is_buddy(&nick));
                                            if let Some(action) = user_menu(hwnd, pt.x, pt.y, op, ignored, buddy) {
                                                app::user_action(hwnd, action, nick);
                                            }
                                            return LRESULT(1);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    return LRESULT(0);
                }
                DefMDIChildProcW(hwnd, msg, wp, lp)
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
                let (dc, t) = (HDC(wp.0 as *mut c_void), crate::look::theme());
                SetBkColor(dc, crate::look::colorref(t.bg));
                SetTextColor(dc, crate::look::colorref(t.fg));
                LRESULT(crate::look::bg_brush().0 as isize)
            }
            WM_DROPFILES => {
                handle_drop(hwnd, windows::Win32::UI::Shell::HDROP(wp.0 as *mut c_void), hwnd);
                LRESULT(0)
            }
            WM_HOVER_CHECK => {
                // the pointer moved: if no link reported itself under it since, the link text goes from the status bar
                if !LINK_SEEN.swap(false, Ordering::Relaxed) && HOVER_SHOWING.swap(false, Ordering::Relaxed) {
                    app::refresh_status();
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                app::child_closing(hwnd);
                DefMDIChildProcW(hwnd, msg, wp, lp)
            }
            WM_DESTROY => {
                remember_child_place(hwnd);
                tab_remove(hwnd);
                DefMDIChildProcW(hwnd, msg, wp, lp)
            }
            WM_NCDESTROY => {
                let r = DefMDIChildProcW(hwnd, msg, wp, lp);
                let p = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut Child;
                if !p.is_null() {
                    drop(Box::from_raw(p));
                }
                app::window_closed(raw(hwnd));
                r
            }
            _ => DefMDIChildProcW(hwnd, msg, wp, lp),
        }
    }
}


fn get_text(h: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(h) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(h, &mut buf) as usize;
        String::from_utf16_lossy(&buf[..got])
    }
}

fn set_text(h: HWND, s: &str) {
    let t = wide(s);
    unsafe {
        let _ = SetWindowTextW(h, PCWSTR(t.as_ptr()));
    }
}

extern "system" fn input_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    parent: usize,
) -> LRESULT {
    unsafe {
        let parent = hw(parent as isize);
        // Any edit other than Tab itself ends the current completion cycle.
        let modifier = [VK_TAB.0, VK_SHIFT.0, VK_CONTROL.0, VK_MENU.0].contains(&(wp.0 as u16));
        if (msg == WM_CHAR && wp.0 != 9) || msg == WM_LBUTTONDOWN || (msg == WM_KEYDOWN && !modifier) {
            if let Some(c) = child(parent) {
                c.completion = None;
            }
        }
        match msg {
            WM_KEYDOWN if wp.0 == VK_TAB.0 as usize => {
                if let Some(c) = child(parent) {
                    let back = GetKeyState(VK_SHIFT.0 as i32) < 0;
                    let text = get_text(hwnd);
                    if c.completion.is_none() {
                        let mut caret = 0u32;
                        send(hwnd, EM_GETSEL, &mut caret as *mut _ as usize, 0);
                        let ctx = completion_context(c);
                        c.completion = complete::Completion::begin(&text, caret as usize, &ctx);
                    }
                    if let Some(comp) = c.completion.as_mut() {
                        let (line, caret) = comp.step(&text, back);
                        set_text(hwnd, &line);
                        send(hwnd, EM_SETSEL, caret, caret as isize);
                    }
                }
                return LRESULT(0);
            }
            WM_CHAR if wp.0 == 13 => {
                let text = get_text(hwnd);
                set_text(hwnd, "");
                if let Some(c) = child(parent) {
                    if !text.trim().is_empty() {
                        c.history.push(text.clone());
                        if c.history.len() > 200 {
                            c.history.remove(0);
                        }
                    }
                    c.hist_pos = c.history.len();
                }
                if !text.trim().is_empty() {
                    app::submit(parent, text);
                }
                return LRESULT(0);
            }
            WM_CHAR if wp.0 == 9 => return LRESULT(0), // swallow Tab (no beep)
            // several pasted lines are asked about first (the program pastes or sends them itself)
            WM_PASTE if app::paste_requested(parent) => return LRESULT(0),
            WM_DROPFILES => {
                handle_drop(parent, windows::Win32::UI::Shell::HDROP(wp.0 as *mut c_void), hwnd);
                return LRESULT(0);
            }
            WM_KEYDOWN if wp.0 == VK_UP.0 as usize || wp.0 == VK_DOWN.0 as usize => {
                if let Some(c) = child(parent) {
                    let up = wp.0 == VK_UP.0 as usize;
                    if up && c.hist_pos > 0 {
                        c.hist_pos -= 1;
                    } else if !up && c.hist_pos < c.history.len() {
                        c.hist_pos += 1;
                    }
                    let line = c.history.get(c.hist_pos).cloned().unwrap_or_default();
                    set_text(hwnd, &line);
                    send(hwnd, EM_SETSEL, line.encode_utf16().count(), line.encode_utf16().count() as isize);
                }
                return LRESULT(0);
            }
            _ => {}
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}

fn completion_context(c: &Child) -> complete::Context {
    let members: Vec<String> = match c.kind {
        Kind::Channel => c.members.iter().map(|(_, n)| n.clone()).collect(),
        Kind::Query => vec![c.name.clone()],
        Kind::DccChat => vec![c.name.trim_start_matches('=').to_owned()],
        Kind::Server => Vec::new(),
    };
    complete::Context {
        channels: app::channel_names(c.conn),
        nicks: complete::order_nicks(&c.recent, &members),
        my_nick: app::nick_of(c.conn),
        aliases: app::alias_names(),
    }
}

/// Remember that `nick` just spoke in this window (nick completion tries recent speakers first).
/// (channel name, number of users, topic) of a channel window, for the Channels monitor.
pub fn channel_info(h: HWND) -> Option<(String, usize, String)> {
    let c = child(h)?;
    (c.kind == Kind::Channel).then(|| (c.name.clone(), c.members.len(), get_text(c.topic)))
}

/// The members of a channel window as (rank symbol, nick).
pub fn channel_members(h: HWND) -> Vec<(String, String)> {
    child(h).filter(|c| c.kind == Kind::Channel).map(|c| c.members.clone()).unwrap_or_default()
}

/// A short label for a chat window: its kind and name, used where lines from every window are mixed.
pub fn window_label(h: HWND) -> Option<(u32, Kind, String)> {
    child(h).map(|c| (c.conn, c.kind, c.name.clone()))
}

pub fn note_speaker(h: HWND, nick: &str) {
    if let Some(c) = child(h) {
        c.recent.retain(|n| !n.eq_ignore_ascii_case(nick));
        c.recent.insert(0, nick.to_owned());
        c.recent.truncate(50);
    }
}

pub fn child_name(h: HWND, kind: Kind) -> Option<String> {
    child(h).filter(|c| c.kind == kind).map(|c| c.name.clone())
}

// ---------------------------------------------------------------- child operations used by app

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineKind {
    Normal,
    /// Joins, parts, mode changes and other status chatter.
    Info,
    Action,
    Notice,
    Error,
}

impl LineKind {
    /// The colour a kind of line has under the current theme (ordinary lines have the text colour).
    fn base(self) -> (Option<Rgb>, bool) {
        let t = crate::look::theme();
        match self {
            LineKind::Normal => (None, false),
            LineKind::Info => (Some(t.info), false),
            LineKind::Action => (Some(t.action), false),
            LineKind::Notice => (Some(t.notice), false),
            LineKind::Error => (Some(t.error), false),
        }
    }
}

fn colorref(c: Rgb) -> windows::Win32::Foundation::COLORREF {
    windows::Win32::Foundation::COLORREF(c.0 as u32 | (c.1 as u32) << 8 | (c.2 as u32) << 16)
}

/// Append one styled run at the end of a RichEdit control.
unsafe fn append_run(edit: HWND, text: &str, st: &Style, base_fg: Option<Rgb>, base_bold: bool) {
    let theme = crate::look::theme();
    let (mut fg, mut bg) = (st.fg.or(base_fg), st.bg);
    if st.reverse {
        let (f, b) = (fg.unwrap_or(theme.fg), bg.unwrap_or(theme.bg));
        fg = Some(b);
        bg = Some(f);
    }
    // colours that come with the text (mIRC colours, highlights) must be readable on this theme's background
    if bg.is_none() {
        fg = fg.map(|c| theme.readable(c));
    }
    let mut cf = CHARFORMAT2W::default();
    cf.Base.cbSize = std::mem::size_of::<CHARFORMAT2W>() as u32;
    cf.Base.dwMask = CFM_MASK(
        CFM_COLOR.0 | CFM_BACKCOLOR.0 | CFM_BOLD.0 | CFM_ITALIC.0 | CFM_UNDERLINE.0 | CFM_STRIKEOUT.0,
    );
    let mut eff = 0u32;
    if st.bold || base_bold {
        eff |= CFE_BOLD.0;
    }
    if st.italic {
        eff |= CFE_ITALIC.0;
    }
    if st.underline {
        eff |= CFE_UNDERLINE.0;
    }
    if st.strike {
        eff |= CFE_STRIKEOUT.0;
    }
    // ordinary text has the theme's text colour, written out (the automatic colour is always the system's)
    cf.Base.crTextColor = colorref(fg.unwrap_or(theme.fg));
    match bg {
        Some(c) => cf.crBackColor = colorref(c),
        None => eff |= CFE_AUTOBACKCOLOR.0,
    }
    cf.Base.dwEffects = CFE_EFFECTS(eff);
    send(edit, EM_SETSEL, usize::MAX, -1); // caret to end
    send(edit, EM_SETCHARFORMAT, SCF_SELECTION as usize, &cf as *const _ as isize);
    let t = wide(text);
    send(edit, EM_REPLACESEL, 0, t.as_ptr() as isize);
}

unsafe fn at_bottom(edit: HWND) -> bool {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_ALL,
        ..Default::default()
    };
    if GetScrollInfo(edit, SB_VERT, &mut si).is_err() {
        return true;
    }
    si.nPos + si.nPage as i32 >= si.nMax - 1
}

/// Append one line (timestamp, then the IRC-formatted text, then the line break) at the end of a RichEdit.
unsafe fn append_line(edit: HWND, ts: Option<&str>, text: &str, base_fg: Option<Rgb>, base_bold: bool, marks: &highlight::Marks) {
    if let Some(ts) = ts {
        let grey = Style { fg: Some(crate::look::theme().timestamp), ..Style::default() };
        append_run(edit, &format!("[{ts}] "), &grey, None, false);
    }
    for span in highlight::apply(format::parse(text), marks) {
        append_run(edit, &span.text, &span.style, base_fg, base_bold);
    }
    append_run(edit, "\r", &Style::default(), None, false);
}

/// Print every window's text again, in the colours of the current theme (the lines were remembered).
fn reprint_all() {
    let mut wins: Vec<HWND> = Vec::new();
    unsafe {
        if !mdi().0.is_null() {
            let _ = EnumChildWindows(mdi(), Some(collect_children), LPARAM(&mut wins as *mut _ as isize));
        }
        for h in wins {
            let Some(c) = child(h) else { continue };
            let edit = c.output;
            send(edit, WM_SETREDRAW, 0, 0);
            send(edit, EM_SETBKGNDCOLOR, 0, crate::look::colorref(crate::look::theme().bg).0 as isize);
            send(edit, EM_SETSEL, 0, -1);
            let empty = wide("");
            send(edit, EM_REPLACESEL, 0, empty.as_ptr() as isize);
            let lines: Vec<StoredLine> = c.lines.iter().cloned().collect();
            for l in lines {
                let (mut base_fg, mut base_bold) = l.kind.base();
                if let Some(color) = l.marks.line {
                    base_fg = Some(color);
                    base_bold = true;
                }
                append_line(edit, l.ts.as_deref(), &l.text, base_fg, base_bold, &l.marks);
            }
            send(edit, EM_SETSEL, usize::MAX, -1);
            send(edit, WM_VSCROLL, SB_BOTTOM.0 as usize, 0);
            send(edit, WM_SETREDRAW, 1, 0);
            let _ = InvalidateRect(edit, None, true);
        }
    }
}

/// Append a line (`ts` prefix, then IRC-formatted `text`) to a window's output.
/// Stays scrolled to the end only if the user was already at the end, and keeps any
/// text selection so copying isn't disrupted by incoming lines.
pub fn print(h: HWND, ts: Option<&str>, text: &str, kind: LineKind, marks: &highlight::Marks) {
    let Some(c) = child(h) else { return };
    let edit = c.output;
    c.lines.push_back(StoredLine { ts: ts.map(str::to_owned), text: text.to_owned(), kind, marks: marks.clone() });
    while c.lines.len() > STORED_LINES {
        c.lines.pop_front();
    }
    let (mut base_fg, mut base_bold) = kind.base();
    // A whole-line highlight overrides the line's base colour; any match lights the tab up.
    if let Some(color) = marks.line {
        base_fg = Some(color);
        base_bold = true;
    }
    let level = if marks.is_empty() { Activity::from(kind) } else { Activity::Highlight };
    if level > c.activity && active_child() != Some(h) {
        c.activity = level;
        repaint_tabs();
    }
    unsafe {
        let follow = at_bottom(edit);
        let first_line = send(edit, EM_GETFIRSTVISIBLELINE, 0, 0);
        let mut sel = CHARRANGE::default();
        send(edit, EM_EXGETSEL, 0, &mut sel as *mut _ as isize);

        send(edit, WM_SETREDRAW, 0, 0);
        if GetWindowTextLengthW(edit) > 400_000 {
            send(edit, EM_SETSEL, 0, 100_000);
            let empty = wide("");
            send(edit, EM_REPLACESEL, 0, empty.as_ptr() as isize);
            sel = CHARRANGE::default();
        }
        append_line(edit, ts, text, base_fg, base_bold, marks);

        if sel.cpMin != sel.cpMax {
            send(edit, EM_EXSETSEL, 0, &sel as *const _ as isize);
        }
        // EM_REPLACESEL scrolls the caret into view; undo that unless the user was following.
        if follow {
            send(edit, WM_VSCROLL, SB_BOTTOM.0 as usize, 0);
        } else {
            let now = send(edit, EM_GETFIRSTVISIBLELINE, 0, 0);
            if now != first_line {
                send(edit, EM_LINESCROLL, 0, first_line - now);
            }
        }
        send(edit, WM_SETREDRAW, 1, 0);
        let _ = InvalidateRect(edit, None, false);
    }
}

/// `EN_MSGFILTER` (a mouse message the chat text passes on).
const EN_MSGFILTER: u32 = 0x0700;

#[repr(C)]
#[allow(dead_code)] // laid out as the system's MSGFILTER; the position is read with GetMessagePos instead
struct MsgFilter {
    hdr: NMHDR,
    msg: u32,
    wparam: usize,
    lparam: isize,
}

/// Set while a link reported itself under the pointer; the posted check after a mouse move reads it.
static LINK_SEEN: AtomicBool = AtomicBool::new(false);
/// A link description is in the status bar.
static HOVER_SHOWING: AtomicBool = AtomicBool::new(false);

/// The text of a link range.
unsafe fn link_text(edit: HWND, chrg: CHARRANGE) -> String {
    let len = (chrg.cpMax - chrg.cpMin).clamp(0, 2048) as usize;
    let mut buf = vec![0u16; len + 1];
    let mut tr = TEXTRANGEW { chrg, lpstrText: windows::core::PWSTR(buf.as_mut_ptr()) };
    send(edit, EM_GETTEXTRANGE, 0, &mut tr as *mut _ as isize);
    String::from_utf16_lossy(&buf[..len])
}

/// The word of the chat text at a client position given as an `lParam` (x low, y high).
unsafe fn word_under(edit: HWND, lparam: isize) -> Option<String> {
    #[repr(C)]
    struct PointL {
        x: i32,
        y: i32,
    }
    const EM_CHARFROMPOS: u32 = 0x00D7;
    let pt = PointL { x: (lparam & 0xffff) as i16 as i32, y: ((lparam >> 16) & 0xffff) as i16 as i32 };
    let at = send(edit, EM_CHARFROMPOS, 0, &pt as *const PointL as isize);
    if at < 0 {
        return None;
    }
    let from = (at as i32 - 80).max(0);
    let around = link_text(edit, CHARRANGE { cpMin: from, cpMax: at as i32 + 80 });
    // the text of a range counts UTF-16 units; a position in it is the same count
    let units: Vec<u16> = around.encode_utf16().collect();
    let pos = (at as i32 - from) as usize;
    if pos >= units.len() {
        return None;
    }
    let before = String::from_utf16_lossy(&units[..pos]).chars().count();
    crate::linkinfo::word_at(&around, before)
}

/// Open a clicked URL, only for http(s) links (RichEdit also auto-detects file:, mailto: etc.).
unsafe fn open_link(edit: HWND, chrg: CHARRANGE) {
    let mut sel = CHARRANGE::default();
    send(edit, EM_EXGETSEL, 0, &mut sel as *mut _ as isize);
    if sel.cpMin != sel.cpMax {
        return; // user is selecting text, not clicking
    }
    let len = (chrg.cpMax - chrg.cpMin).clamp(0, 2048) as usize;
    let mut buf = vec![0u16; len + 1];
    let mut tr = TEXTRANGEW { chrg, lpstrText: windows::core::PWSTR(buf.as_mut_ptr()) };
    send(edit, EM_GETTEXTRANGE, 0, &mut tr as *mut _ as isize);
    let url = String::from_utf16_lossy(&buf[..len]);
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let u = wide(&url);
        ShellExecuteW(None, w!("open"), PCWSTR(u.as_ptr()), None, None, SW_SHOWNORMAL);
    }
}

/// Is one of our windows the foreground window?
pub fn app_is_foreground() -> bool {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        pid == GetCurrentProcessId()
    }
}

/// Flash the taskbar button until the user switches back to the app.
pub fn flash_taskbar() {
    let info = FLASHWINFO {
        cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
        hwnd: frame(),
        dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
        uCount: 0,
        dwTimeout: 0,
    };
    unsafe {
        let _ = FlashWindowEx(&info);
    }
}

pub fn beep() {
    unsafe {
        let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(MB_ICONASTERISK);
    }
}

pub fn set_timestamps_checked(on: bool) {
    unsafe {
        let bar = HMENU(MENU_BAR.load(Ordering::Relaxed) as *mut c_void);
        let flag = if on { MF_CHECKED } else { MF_UNCHECKED };
        let _ = CheckMenuItem(bar, ID_OPT_TIMESTAMPS as u32, (MF_BYCOMMAND | flag).0);
    }
}

#[repr(C)]
struct CharRange {
    min: i32,
    max: i32,
}

#[repr(C)]
struct FindTextEx {
    range: CharRange,
    text: *const u16,
    found: CharRange,
}

/// Find `text` in the chat text of the active window, from the end of the selection (or its start, going up),
/// wrapping once; the match is selected and scrolled into view. False when there is none.
pub fn find_in_output(text: &str, match_case: bool, forward: bool) -> bool {
    const EM_EXGETSEL: u32 = 0x0434;
    const EM_EXSETSEL: u32 = 0x0437;
    const EM_FINDTEXTEXW: u32 = 0x047C;
    const EM_HIDESELECTION: u32 = 0x043F;
    const EM_SCROLLCARET: u32 = 0x00B7;
    const FR_DOWN: usize = 1;
    const FR_MATCHCASE: usize = 4;
    let Some(c) = active_child().and_then(|h| child(h)) else { return false };
    if text.is_empty() || c.output.0.is_null() {
        return false;
    }
    let needle = wide(text);
    let flags = (if forward { FR_DOWN } else { 0 }) | (if match_case { FR_MATCHCASE } else { 0 });
    let mut sel = CharRange { min: 0, max: 0 };
    send(c.output, EM_EXGETSEL, 0, &mut sel as *mut CharRange as isize);
    let try_find = |from: i32, to: i32| -> Option<(i32, i32)> {
        let mut f = FindTextEx { range: CharRange { min: from, max: to }, text: needle.as_ptr(), found: CharRange { min: 0, max: 0 } };
        let at = send(c.output, EM_FINDTEXTEXW, flags, &mut f as *mut FindTextEx as isize);
        (at >= 0).then_some((f.found.min, f.found.max))
    };
    let found = if forward {
        try_find(sel.max, -1).or_else(|| try_find(0, -1))
    } else {
        try_find(sel.min, 0).or_else(|| try_find(-1, 0))
    };
    match found {
        Some((a, b)) => {
            let mut r = CharRange { min: a, max: b };
            send(c.output, EM_EXSETSEL, 0, &mut r as *mut CharRange as isize);
            send(c.output, EM_HIDESELECTION, 0, 0); // keep the match shown while the entry box has the focus
            send(c.output, EM_SCROLLCARET, 0, 0);
            true
        }
        None => false,
    }
}

pub fn set_typing_checked(show: bool, send: bool) {
    unsafe {
        let bar = HMENU(MENU_BAR.load(Ordering::Relaxed) as *mut c_void);
        for (id, on) in [(ID_OPT_TYPING_SHOW, show), (ID_OPT_TYPING_SEND, send)] {
            let flag = if on { MF_CHECKED } else { MF_UNCHECKED };
            let _ = CheckMenuItem(bar, id as u32, (MF_BYCOMMAND | flag).0);
        }
    }
}

/// The entry box of a channel or chat window changed.
fn typing_edited(h: HWND) {
    let Some(c) = child(h) else { return };
    if !matches!(c.kind, Kind::Channel | Kind::Query) {
        return;
    }
    let text = get_text(c.input);
    if let Some(state) = c.typing_out.edited(crate::typing::now_ms(), &text) {
        app::send_typing(c.conn, &c.name.clone(), state);
    }
}

/// Once a second for one window: say `paused` after a pause in our typing, and forget typing that ran out.
/// True when the list of people typing here changed.
pub fn typing_tick_window(h: HWND) -> bool {
    let Some(c) = child(h) else { return false };
    if let Some(state) = c.typing_out.tick(crate::typing::now_ms()) {
        app::send_typing(c.conn, &c.name.clone(), state);
    }
    c.typing_in.expire(crate::typing::now_ms())
}

/// `nick` said something about their typing in this window. True when the list of typists changed.
pub fn typing_in(h: HWND, nick: &str, state: chimerairc_core::TypingState) -> bool {
    child(h).is_some_and(|c| c.typing_in.set(crate::typing::now_ms(), nick, state))
}

/// `nick` sent a message here, so is no longer typing. True when the list changed.
pub fn typing_spoke(h: HWND, nick: &str) -> bool {
    child(h).is_some_and(|c| c.typing_in.spoke(nick))
}

/// `alice is typing...` for this window, or an empty string.
pub fn typing_text(h: HWND) -> String {
    child(h).map(|c| c.typing_in.text()).unwrap_or_default()
}

pub fn set_topic(h: HWND, topic: &str) {
    if let Some(c) = child(h) {
        if !c.topic.0.is_null() {
            set_text(c.topic, &format::strip(topic));
        }
    }
}

pub fn set_members(h: HWND, members: &[(String, String)]) {
    let Some(c) = child(h) else { return };
    if c.list.0.is_null() {
        return;
    }
    c.members = members.to_vec();
    unsafe {
        send(c.list, WM_SETREDRAW, 0, 0);
        send(c.list, LB_RESETCONTENT, 0, 0);
        for (sym, nick) in members {
            let t = wide(&crate::capsfmt::member_text(sym, nick, c.away.contains(&nick.to_lowercase())));
            send(c.list, LB_ADDSTRING, 0, t.as_ptr() as isize);
        }
        send(c.list, WM_SETREDRAW, 1, 0);
        let _ = InvalidateRect(c.list, None, true);
    }
}

/// `away-notify`: mark `nick` as away (or back) in this window's nick list. Only the one entry is redone, so a
/// busy channel's list does not flicker or lose its scroll position.
pub fn set_away(h: HWND, nick: &str, away: bool) {
    let Some(c) = child(h) else { return };
    if c.kind != Kind::Channel {
        return;
    }
    let key = nick.to_lowercase();
    let changed = if away { c.away.insert(key.clone()) } else { c.away.remove(&key) };
    if !changed || c.list.0.is_null() {
        return;
    }
    if let Some(i) = c.members.iter().position(|(_, m)| m.to_lowercase() == key) {
        let (sym, m) = c.members[i].clone();
        let t = wide(&crate::capsfmt::member_text(&sym, &m, away));
        unsafe {
            let top = send(c.list, LB_GETTOPINDEX, 0, 0);
            let sel = send(c.list, LB_GETCURSEL, 0, 0);
            send(c.list, WM_SETREDRAW, 0, 0);
            send(c.list, LB_DELETESTRING, i, 0);
            send(c.list, LB_INSERTSTRING, i, t.as_ptr() as isize);
            if sel == i as isize {
                send(c.list, LB_SETCURSEL, i, 0);
            }
            send(c.list, LB_SETTOPINDEX, top.max(0) as usize, 0);
            send(c.list, WM_SETREDRAW, 1, 0);
            let _ = InvalidateRect(c.list, None, true);
        }
    }
}

pub fn is_channel_window(h: HWND) -> bool {
    child(h).is_some_and(|c| c.kind == Kind::Channel)
}

pub fn has_member(h: HWND, nick: &str) -> bool {
    let n = nick.to_lowercase();
    child(h).is_some_and(|c| c.members.iter().any(|(_, m)| m.to_lowercase() == n))
}

/// True if `nick` holds half-op or better in this channel window (so op commands are offered).
fn can_op(c: &Child, nick: &str) -> bool {
    let n = nick.to_lowercase();
    c.members.iter().any(|(sym, m)| m.to_lowercase() == n && sym.starts_with(['~', '&', '@', '%']))
}

fn kind_is_channel(c: &Child) -> bool {
    c.kind == Kind::Channel
}

/// What the clipboard holds that a paste into the entry box cares about.
pub enum Clip {
    Text(String),
    /// A picture: the DIB (`BITMAPINFOHEADER` and pixels).
    Picture(Vec<u8>),
    Nothing,
}

/// Read the clipboard: its text, else its picture.
pub fn clipboard_content() -> Clip {
    use windows::Win32::System::DataExchange::{GetClipboardData, IsClipboardFormatAvailable};
    use windows::Win32::System::Memory::GlobalSize;
    const CF_UNICODETEXT: u32 = 13;
    const CF_DIB: u32 = 8;
    unsafe {
        if OpenClipboard(frame()).is_err() {
            return Clip::Nothing;
        }
        let mut out = Clip::Nothing;
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() {
            if let Ok(h) = GetClipboardData(CF_UNICODETEXT) {
                let mem = windows::Win32::Foundation::HGLOBAL(h.0);
                let p = GlobalLock(mem) as *const u16;
                if !p.is_null() {
                    let max = GlobalSize(mem) / 2;
                    let mut n = 0;
                    while n < max && *p.add(n) != 0 {
                        n += 1;
                    }
                    out = Clip::Text(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
                    let _ = GlobalUnlock(mem);
                }
            }
        } else if IsClipboardFormatAvailable(CF_DIB).is_ok() {
            if let Ok(h) = GetClipboardData(CF_DIB) {
                let mem = windows::Win32::Foundation::HGLOBAL(h.0);
                let p = GlobalLock(mem) as *const u8;
                if !p.is_null() {
                    let n = GlobalSize(mem).min(512 << 20);
                    out = Clip::Picture(std::slice::from_raw_parts(p, n).to_vec());
                    let _ = GlobalUnlock(mem);
                }
            }
        }
        let _ = CloseClipboard();
        out
    }
}

/// Put text into the entry box at the caret (replacing the selection).
pub fn input_insert(h: HWND, text: &str) {
    if let Some(c) = child(h) {
        let w = wide(text);
        send(c.input, 0x00C2 /* EM_REPLACESEL */, 1, w.as_ptr() as isize);
    }
}

/// The nick in the user list of channel window `h` under the screen point.
pub fn user_at_screen(h: HWND, x: i32, y: i32) -> Option<String> {
    let c = child(h)?;
    if c.kind != Kind::Channel || c.list.0.is_null() {
        return None;
    }
    unsafe { user_at(c, x, y).map(|(nick, _, _)| nick) }
}

/// Files were dropped on `source` (the chat window or one of its parts): hand them to the program.
unsafe fn handle_drop(window: HWND, hdrop: windows::Win32::UI::Shell::HDROP, source: HWND) {
    use windows::Win32::UI::Shell::{DragFinish, DragQueryFileW, DragQueryPoint};
    let mut pt = windows::Win32::Foundation::POINT::default();
    let _ = DragQueryPoint(hdrop, &mut pt);
    let _ = ClientToScreen(source, &mut pt);
    let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None) as usize;
    let mut paths = Vec::new();
    for i in 0..count.min(crate::paste::MAX_DROPPED_FILES + 1) {
        let len = DragQueryFileW(hdrop, i as u32, None) as usize;
        let mut buf = vec![0u16; len + 1];
        DragQueryFileW(hdrop, i as u32, Some(&mut buf));
        paths.push(std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len])));
    }
    DragFinish(hdrop);
    app::files_dropped(window, paths, count, pt.x, pt.y);
}

/// The chat text passes dropped files on to its window.
extern "system" fn drop_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, parent: usize) -> LRESULT {
    unsafe {
        if msg == WM_DROPFILES {
            handle_drop(hw(parent as isize), windows::Win32::UI::Shell::HDROP(wp.0 as *mut c_void), h);
            return LRESULT(0);
        }
        DefSubclassProc(h, msg, wp, lp)
    }
}

/// Copy text to the clipboard as Unicode.
pub fn copy_to_clipboard(text: &str) {
    unsafe {
        if OpenClipboard(frame()).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        let w = wide(text);
        let bytes = w.len() * 2;
        if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, bytes) {
            let p = GlobalLock(mem) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(mem);
                const CF_UNICODETEXT: u32 = 13;
                if SetClipboardData(CF_UNICODETEXT, windows::Win32::Foundation::HANDLE(mem.0)).is_err() {
                    let _ = windows::Win32::Foundation::GlobalFree(windows::Win32::Foundation::HGLOBAL(mem.0)); // clipboard did not take ownership
                }
            }
        }
        let _ = CloseClipboard();
    }
}

/// Show the user-list menu at screen point (x, y) and return the chosen action.
unsafe fn user_menu(owner: HWND, x: i32, y: i32, op: bool, ignored: bool, buddy: bool) -> Option<UserAction> {
    let m = CreatePopupMenu().ok()?;
    let add = |m: HMENU, id: usize, t: PCWSTR| {
        let _ = AppendMenuW(m, MF_STRING, id, t);
    };
    add(m, UM_QUERY, w!("&Query"));
    add(m, UM_WHOIS, w!("&Whois"));
    add(m, UM_SENDFILE, w!("Send &File..."));
    add(m, UM_DCCCHAT, w!("&DCC Chat"));
    let ctcp = CreatePopupMenu().ok()?;
    add(ctcp, UM_CTCP_VERSION, w!("&Version"));
    add(ctcp, UM_CTCP_PING, w!("&Ping"));
    add(ctcp, UM_CTCP_TIME, w!("&Time"));
    let _ = AppendMenuW(m, MF_POPUP, ctcp.0 as usize, w!("&CTCP"));
    if op {
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        let ops = CreatePopupMenu().ok()?;
        add(ops, UM_OP, w!("Give &Op"));
        add(ops, UM_DEOP, w!("&Take Op"));
        add(ops, UM_VOICE, w!("Give &Voice"));
        add(ops, UM_DEVOICE, w!("Take V&oice"));
        let _ = AppendMenuW(m, MF_POPUP, ops.0 as usize, w!("O&p"));
        add(m, UM_KICK, w!("&Kick"));
        add(m, UM_BAN, w!("&Ban (nick!*@*)"));
        add(m, UM_KICKBAN, w!("Kick + Ba&n"));
    }
    let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
    if ignored {
        add(m, UM_UNIGNORE, w!("&Unignore"));
    } else {
        add(m, UM_IGNORE, w!("&Ignore"));
    }
    if buddy {
        add(m, UM_BUDDY_DEL, w!("Remove from &Buddies"));
    } else {
        add(m, UM_BUDDY_ADD, w!("Add to &Buddies"));
    }
    add(m, UM_COPY, w!("Cop&y Nick"));
    let _ = SetMenuDefaultItem(m, UM_QUERY as u32, 0);
    let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, x, y, 0, owner, None).0 as usize;
    let _ = DestroyMenu(m); // also destroys the submenus
    Some(match cmd {
        UM_QUERY => UserAction::Query,
        UM_WHOIS => UserAction::Whois,
        UM_CTCP_VERSION => UserAction::CtcpVersion,
        UM_CTCP_PING => UserAction::CtcpPing,
        UM_CTCP_TIME => UserAction::CtcpTime,
        UM_OP => UserAction::Op,
        UM_DEOP => UserAction::Deop,
        UM_VOICE => UserAction::Voice,
        UM_DEVOICE => UserAction::Devoice,
        UM_KICK => UserAction::Kick,
        UM_BAN => UserAction::Ban,
        UM_KICKBAN => UserAction::KickBan,
        UM_COPY => UserAction::CopyNick,
        UM_SENDFILE => UserAction::SendFile,
        UM_DCCCHAT => UserAction::DccChat,
        UM_IGNORE => UserAction::Ignore,
        UM_UNIGNORE => UserAction::Unignore,
        UM_BUDDY_ADD => UserAction::AddBuddy,
        UM_BUDDY_DEL => UserAction::RemoveBuddy,
        _ => return None,
    })
}

/// Nick of the user-list row under screen point (x, y), selecting it; `None` if not on a row.
/// With (-1, -1) (keyboard invoked) the current selection is used and the menu is placed at the row.
unsafe fn user_at(c: &Child, x: i32, y: i32) -> Option<(String, i32, i32)> {
    let (idx, sx, sy) = if x == -1 && y == -1 {
        let sel = send(c.list, LB_GETCURSEL, 0, 0);
        if sel < 0 {
            return None;
        }
        let mut r = RECT::default();
        send(c.list, LB_GETITEMRECT, sel as usize, &mut r as *mut _ as isize);
        let mut pt = windows::Win32::Foundation::POINT { x: r.left + 8, y: r.bottom };
        let _ = ClientToScreen(c.list, &mut pt);
        (sel as usize, pt.x, pt.y)
    } else {
        let mut pt = windows::Win32::Foundation::POINT { x, y };
        let _ = ScreenToClient(c.list, &mut pt);
        let r = send(c.list, LB_ITEMFROMPOINT, 0, ((pt.y as isize & 0xffff) << 16) | (pt.x as isize & 0xffff));
        if (r >> 16) & 0xffff != 0 {
            return None; // outside the client area / past the last row
        }
        (r as usize & 0xffff, x, y)
    };
    send(c.list, LB_SETCURSEL, idx, 0);
    c.members.get(idx).map(|(_, n)| (n.clone(), sx, sy))
}

pub fn clear_members(h: HWND) {
    set_members(h, &[]);
}
