//! Puts the theme, the chat colours and the fonts on the screen: the colour brushes, the dark title bar
//! and dark scroll bars, the controls' colours and the font choosers.

use crate::theme::{Choice, FontSpec, Overrides, Theme};
use chimerairc_proto::format::Rgb;
use std::cell::RefCell;
use std::ffi::c_void;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Controls::Dialogs::{ChooseFontW, CHOOSEFONTW, CF_INITTOLOGFONTSTRUCT, CF_LIMITSIZE, CF_SCREENFONTS};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::*;

struct Look {
    choice: Choice,
    overrides: Overrides,
    theme: Theme,
    chat_font: FontSpec,
    ui_font: FontSpec,
    /// The brush of panel colour and of the text background (HBRUSH values).
    panel: isize,
    bg: isize,
}

thread_local! {
    static LOOK: RefCell<Option<Look>> = const { RefCell::new(None) };
}

pub fn colorref(c: Rgb) -> COLORREF {
    COLORREF(c.0 as u32 | (c.1 as u32) << 8 | (c.2 as u32) << 16)
}

fn make_brush(c: Rgb) -> isize {
    unsafe { CreateSolidBrush(colorref(c)).0 as isize }
}

/// Is Windows set to dark apps?
fn system_dark() -> bool {
    unsafe {
        let mut value: u32 = 1;
        let mut size = 4u32;
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        );
        r.is_ok() && value == 0
    }
}

/// Set up the look before any window exists.
pub fn init(choice: Choice, overrides: Overrides, chat_font: FontSpec, ui_font: FontSpec) {
    let theme = Theme::resolve(choice, system_dark(), &overrides);
    let (panel, bg) = (make_brush(theme.panel), make_brush(theme.bg));
    LOOK.with(|l| *l.borrow_mut() = Some(Look { choice, overrides, theme, chat_font, ui_font, panel, bg }));
    prefer_dark_popups(theme.dark);
}

pub fn theme() -> Theme {
    LOOK.with(|l| l.borrow().as_ref().map(|l| l.theme)).unwrap_or(Theme::LIGHT)
}

pub fn choice() -> Choice {
    LOOK.with(|l| l.borrow().as_ref().map(|l| l.choice)).unwrap_or_default()
}

pub fn chat_font() -> FontSpec {
    LOOK.with(|l| l.borrow().as_ref().map(|l| l.chat_font.clone())).unwrap_or_else(FontSpec::chat_default)
}

pub fn ui_font() -> FontSpec {
    LOOK.with(|l| l.borrow().as_ref().map(|l| l.ui_font.clone())).unwrap_or_else(FontSpec::ui_default)
}

/// The brush for panels and window backgrounds.
pub fn panel_brush() -> HBRUSH {
    HBRUSH(LOOK.with(|l| l.borrow().as_ref().map(|l| l.panel)).unwrap_or(0) as *mut c_void)
}

/// The brush for the background of text, lists and entry boxes.
pub fn bg_brush() -> HBRUSH {
    HBRUSH(LOOK.with(|l| l.borrow().as_ref().map(|l| l.bg)).unwrap_or(0) as *mut c_void)
}

/// A font for the given spec at the system's scaling.
pub fn make_font(spec: &FontSpec, bold: bool) -> HFONT {
    let dpi = unsafe { GetDpiForSystem() } as i32;
    let height = -(spec.points as i32 * dpi.max(96) / 72);
    let weight = if bold || spec.bold { FW_BOLD.0 } else { FW_NORMAL.0 } as i32;
    let face: Vec<u16> = spec.face.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        CreateFontW(
            height, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32, 0, PCWSTR(face.as_ptr()),
        )
    }
}

// ---------------------------------------------------------------- changing the look

/// Re-resolve the theme after a change and put it on every window.
fn rebuild(f: impl FnOnce(&mut Look)) {
    LOOK.with(|l| {
        if let Some(look) = l.borrow_mut().as_mut() {
            f(look);
            look.theme = Theme::resolve(look.choice, system_dark(), &look.overrides);
            let (old_panel, old_bg) = (look.panel, look.bg);
            look.panel = make_brush(look.theme.panel);
            look.bg = make_brush(look.theme.bg);
            // the class backgrounds that used the old brushes are replaced in `apply`; free the old ones after
            OLD_BRUSHES.with(|o| o.borrow_mut().extend([old_panel, old_bg]));
            prefer_dark_popups(look.theme.dark);
        }
    });
}

thread_local! {
    static OLD_BRUSHES: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
}

pub fn set_choice(choice: Choice) {
    rebuild(|l| l.choice = choice);
    apply();
}

pub fn set_overrides(o: Overrides) {
    rebuild(|l| l.overrides = o);
    apply();
}

pub fn set_fonts(chat: FontSpec, ui: FontSpec) {
    LOOK.with(|l| {
        if let Some(look) = l.borrow_mut().as_mut() {
            look.chat_font = chat;
            look.ui_font = ui;
        }
    });
    crate::ui::fonts_changed();
}

/// Put the current look on every window of the program.
pub fn apply() {
    unsafe {
        let frame = crate::ui::frame();
        if frame.0.is_null() {
            return;
        }
        extern "system" fn each(h: HWND, _: LPARAM) -> BOOL {
            style_tree(h);
            true.into()
        }
        let _ = EnumThreadWindows(GetCurrentThreadId(), Some(each), LPARAM(0));
        crate::ui::theme_changed();
        let _ = RedrawWindow(frame, None, None, RDW_ERASE | RDW_FRAME | RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW);
        // the old brushes are no longer used anywhere
        OLD_BRUSHES.with(|o| {
            for b in o.borrow_mut().drain(..) {
                if b != 0 {
                    let _ = DeleteObject(HGDIOBJ(b as *mut c_void));
                }
            }
        });
    }
}

// ---------------------------------------------------------------- styling windows

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n.min(64)])
}

fn set_theme(h: HWND, dark_name: PCWSTR, light_name: PCWSTR) {
    let name = if theme().dark { dark_name } else { light_name };
    unsafe {
        let _ = SetWindowTheme(h, name, PCWSTR::null());
    }
}

/// Make the title bar dark or light to match the theme.
pub fn style_title_bar(hwnd: HWND) {
    let dark: BOOL = theme().dark.into();
    unsafe {
        // 20 is the documented attribute (Windows 11 and Windows 10 20H1 on), 19 the earlier undocumented one
        for attr in [20, 19] {
            let _ = DwmSetWindowAttribute(hwnd, windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(attr), &dark as *const BOOL as *const c_void, 4);
        }
    }
}

/// Colour one window by what kind of control it is.
fn style_one(h: HWND) {
    let t = theme();
    let (bg, fg) = (colorref(t.bg), colorref(t.fg));
    let class = class_of(h);
    unsafe {
        // The system redraws the sunken edge of the MDI area and of its windows in white whenever the program is
        // activated or deactivated, whatever is painted over it: in the dark theme they get no system edge at all.
        if matches!(class.as_str(), "MDIClient" | "ChimeraIRC.Child") {
            use windows::Win32::UI::WindowsAndMessaging::*;
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            let edges = WS_EX_CLIENTEDGE.0 | WS_EX_WINDOWEDGE.0;
            let new = ex & !edges;
            if new != ex {
                SetWindowLongW(h, GWL_EXSTYLE, new as i32);
                let _ = SetWindowPos(h, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        match class.as_str() {
            "SysListView32" => {
                SendMessageW(h, LVM_SETBKCOLOR, WPARAM(0), LPARAM(bg.0 as isize));
                SendMessageW(h, LVM_SETTEXTBKCOLOR, WPARAM(0), LPARAM(bg.0 as isize));
                SendMessageW(h, LVM_SETTEXTCOLOR, WPARAM(0), LPARAM(fg.0 as isize));
                set_theme(h, w!("DarkMode_Explorer"), w!("Explorer"));
                let header = HWND(SendMessageW(h, LVM_GETHEADER, WPARAM(0), LPARAM(0)).0 as *mut c_void);
                if !header.0.is_null() {
                    set_theme(header, w!("DarkMode_ItemsView"), w!("Explorer"));
                    let _ = windows::Win32::UI::Shell::SetWindowSubclass(header, Some(header_proc), 78, 0);
                    let _ = InvalidateRect(header, None, true);
                }
                let _ = InvalidateRect(h, None, true);
            }
            "SysTreeView32" => {
                SendMessageW(h, TVM_SETBKCOLOR, WPARAM(0), LPARAM(bg.0 as isize));
                SendMessageW(h, TVM_SETTEXTCOLOR, WPARAM(0), LPARAM(fg.0 as isize));
                SendMessageW(h, TVM_SETLINECOLOR, WPARAM(0), LPARAM(colorref(t.timestamp).0 as isize));
                set_theme(h, w!("DarkMode_Explorer"), w!("Explorer"));
            }
            "RICHEDIT50W" | "RichEdit50W" => {
                SendMessageW(h, windows::Win32::UI::Controls::RichEdit::EM_SETBKGNDCOLOR, WPARAM(0), LPARAM(bg.0 as isize));
                set_theme(h, w!("DarkMode_Explorer"), w!("Explorer"));
            }
            "Edit" | "ListBox" | "ScrollBar" => set_theme(h, w!("DarkMode_Explorer"), w!("Explorer")),
            _ => {}
        }
        // the sunken 3D edge of boxes is white and grey: in the dark theme it is repainted flat
        if matches!(class.as_str(), "Edit" | "ListBox" | "SysListView32" | "SysTreeView32" | "RICHEDIT50W" | "RichEdit50W" | "MDIClient") {
            let _ = windows::Win32::UI::Shell::SetWindowSubclass(h, Some(edge_proc), 79, 0);
            let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                h, None, 0, 0, 0, 0,
                windows::Win32::UI::WindowsAndMessaging::SWP_FRAMECHANGED | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                    | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE | windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER
                    | windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
            );
        }
        match class.as_str() {
            "SysTabControl32" => {
                // paint the blank strip beside the tabs in the panel colour
                let _ = windows::Win32::UI::Shell::SetWindowSubclass(h, Some(tab_proc), 77, 0);
                let _ = InvalidateRect(h, None, true);
            }
            "msctls_statusbar32" => {
                let _ = windows::Win32::UI::Shell::SetWindowSubclass(h, Some(status_proc), 80, 0);
                SendMessageW(h, 0x2001 /* SB_SETBKCOLOR */, WPARAM(0), LPARAM(colorref(t.panel).0 as isize));
            }
            "MDIClient" => {
                SetClassLongPtrW(h, GCLP_HBRBACKGROUND, panel_brush().0 as isize);
            }
            "ChimeraIRC.Child" => {
                let _ = windows::Win32::UI::Shell::SetWindowSubclass(h, Some(child_proc), 81, 0);
                // no visual style for the frame: the style gives the title bar rounded top corners (a window region)
                // that would cut the corners of the flat title bar drawn here
                let _ = SetWindowTheme(h, w!(""), w!(""));
                let _ = windows::Win32::Graphics::Gdi::SetWindowRgn(h, None, true);
                SetClassLongPtrW(h, GCLP_HBRBACKGROUND, panel_brush().0 as isize);
                let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                    h, None, 0, 0, 0, 0,
                    windows::Win32::UI::WindowsAndMessaging::SWP_FRAMECHANGED | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE | windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER
                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
                );
            }
            // the program's own windows (but not the forms and the drop marker): panel colour behind the controls
            c if c.starts_with("ChimeraIRC.") && c != "ChimeraIRC.Form" && c != "ChimeraIRC.DropHint" => {
                SetClassLongPtrW(h, GCLP_HBRBACKGROUND, panel_brush().0 as isize);
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn tab_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{LRESULT, RECT};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    if msg == WM_ERASEBKGND {
        let mut rc = RECT::default();
        let _ = GetClientRect(h, &mut rc);
        FillRect(HDC(wp.0 as *mut c_void), &rc, panel_brush());
        return LRESULT(1);
    }
    if msg == WM_PAINT {
        // the control draws a white 3D frame round every tab: in the dark theme paint them all here, through the
        // owner (which draws the tabs' contents) with nothing else
        let parent = GetParent(h).unwrap_or_default();
        let id = GetDlgCtrlID(h) as usize;
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(h, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(h, &mut client);
        FillRect(dc, &client, panel_brush());
        let count = SendMessageW(h, 0x1304 /* TCM_GETITEMCOUNT */, WPARAM(0), LPARAM(0)).0.max(0) as usize;
        let current = SendMessageW(h, 0x130B /* TCM_GETCURSEL */, WPARAM(0), LPARAM(0)).0;
        for i in 0..count {
            let mut rc = RECT::default();
            if SendMessageW(h, 0x130A /* TCM_GETITEMRECT */, WPARAM(i), LPARAM(&mut rc as *mut RECT as isize)).0 == 0 {
                continue;
            }
            let dis = DRAWITEMSTRUCT {
                CtlType: ODT_TAB,
                CtlID: id as u32,
                itemID: i as u32,
                itemAction: ODA_DRAWENTIRE,
                itemState: if current == i as isize { ODS_SELECTED } else { ODS_DEFAULT },
                hwndItem: h,
                hDC: dc,
                rcItem: rc,
                itemData: 0,
            };
            SendMessageW(parent, WM_DRAWITEM, WPARAM(id), LPARAM(&dis as *const DRAWITEMSTRUCT as isize));
            let line = CreateSolidBrush(colorref(theme().border));
            FrameRect(dc, &rc, line);
            let _ = DeleteObject(HGDIOBJ(line.0));
        }
        let _ = EndPaint(h, &ps);
        return LRESULT(0);
    }
    windows::Win32::UI::Shell::DefSubclassProc(h, msg, wp, lp)
}

/// What `WM_GETTITLEBARINFOEX` fills in: where the title bar and its buttons are (screen coordinates).
#[repr(C)]
struct TitleBarInfoEx {
    size: u32,
    bar: windows::Win32::Foundation::RECT,
    state: [u32; 6],
    rects: [windows::Win32::Foundation::RECT; 6],
}

/// Draw the title bar and the frame of a chat window that is not maximized: flat, in the theme's colours, with
/// the icon, the title and the three buttons. The system still decides what a click on them does.
unsafe fn paint_caption_frame(h: HWND) {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let t = theme();
    let mut win = RECT::default();
    let mut cl = RECT::default();
    let mut origin = POINT { x: 0, y: 0 };
    if GetWindowRect(h, &mut win).is_err() || GetClientRect(h, &mut cl).is_err() || !ClientToScreen(h, &mut origin).as_bool() {
        return;
    }
    let (w, ht) = (win.right - win.left, win.bottom - win.top);
    if w < 8 || ht < 8 {
        return;
    }
    let dc = GetWindowDC(h);
    if dc.is_invalid() {
        return;
    }
    let (l, tp) = (origin.x - win.left, origin.y - win.top);
    // the frame: the panel colour with a thin line outside
    if !IsIconic(h).as_bool() {
        ExcludeClipRect(dc, l, tp, l + cl.right, tp + cl.bottom);
    }
    let whole = RECT { left: 0, top: 0, right: w, bottom: ht };
    FillRect(dc, &whole, panel_brush());
    let line = CreateSolidBrush(colorref(t.border));
    FrameRect(dc, &whole, line);
    let _ = DeleteObject(HGDIOBJ(line.0));

    // the title bar
    let mut info = TitleBarInfoEx { size: std::mem::size_of::<TitleBarInfoEx>() as u32, bar: RECT::default(), state: [0; 6], rects: [RECT::default(); 6] };
    SendMessageW(h, 0x033F /* WM_GETTITLEBARINFOEX */, WPARAM(0), LPARAM(&mut info as *mut TitleBarInfoEx as isize));
    let mut bar = info.bar;
    if bar.right <= bar.left || bar.bottom <= bar.top {
        ReleaseDC(h, dc);
        return;
    }
    let _ = OffsetRect(&mut bar, -win.left, -win.top);
    // the system's title bar rectangle leaves out the window-menu icon at the left: the caption spans the whole width
    bar.left = 1;
    bar.right = w - 1;
    let active = !GetPropW(h, w!("chimera.active")).is_invalid();
    let (bg, fg) = if active { (t.accent, t.accent_text) } else { (t.panel, t.timestamp) };
    let brush = CreateSolidBrush(colorref(bg));
    FillRect(dc, &bar, brush);
    let _ = DeleteObject(HGDIOBJ(brush.0));
    let rule = CreateSolidBrush(colorref(t.border));
    FillRect(dc, &RECT { left: bar.left, top: bar.bottom - 1, right: bar.right, bottom: bar.bottom }, rule);
    let _ = DeleteObject(HGDIOBJ(rule.0));

    let bh = bar.bottom - bar.top;
    // the buttons: Marlett is the font the system draws its caption buttons with (0 minimize, 1 maximize,
    // 2 restore, r close)
    let face = crate::util::wide("Marlett");
    let marlett = CreateFontW(-(bh * 5 / 10).max(8), 0, 0, 0, 400, 0, 0, 0, 2, 0, 0, 0, 0, PCWSTR(face.as_ptr()));
    let old = SelectObject(dc, HGDIOBJ(marlett.0));
    SetBkMode(dc, TRANSPARENT);
    let iconic = IsIconic(h).as_bool();
    let mut first_button = bar.right;
    for (slot, glyph) in [(2usize, if iconic { '2' } else { '0' }), (3, '1'), (5, 'r')] {
        let state = info.state[slot];
        if state & 0x8000 != 0 {
            continue; // invisible
        }
        let mut r = info.rects[slot];
        let _ = OffsetRect(&mut r, -win.left, -win.top);
        if r.right <= r.left {
            continue;
        }
        first_button = first_button.min(r.left);
        SetTextColor(dc, colorref(if state & 1 != 0 { t.timestamp } else { fg }));
        let mut g = [glyph as u16];
        DrawTextW(dc, &mut g, &mut r, DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
    }
    SelectObject(dc, old);
    let _ = DeleteObject(HGDIOBJ(marlett.0));

    // the small icon, then the title
    let mut x = bar.left + crate::util::px(6);
    let mut icon = HICON(SendMessageW(h, WM_GETICON, WPARAM(ICON_SMALL as usize), LPARAM(0)).0 as *mut c_void);
    if icon.0.is_null() {
        icon = HICON(GetClassLongPtrW(h, GCLP_HICONSM) as *mut c_void);
    }
    let side = crate::util::px(16);
    if !icon.0.is_null() {
        let _ = DrawIconEx(dc, x, bar.top + (bh - side) / 2, icon, side, side, 0, None, DI_NORMAL);
        x += side + crate::util::px(6);
    }
    let mut buf = [0u16; 200];
    let n = GetWindowTextW(h, &mut buf).max(0) as usize;
    let font = HGDIOBJ(crate::ui::ui_font_raw() as *mut c_void);
    let old = SelectObject(dc, font);
    SetTextColor(dc, colorref(fg));
    let mut tr = RECT { left: x, top: bar.top, right: first_button - crate::util::px(4), bottom: bar.bottom };
    if n > 0 && tr.right > tr.left {
        DrawTextW(dc, &mut buf[..n], &mut tr, DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
    }
    SelectObject(dc, old);
    ReleaseDC(h, dc);
}

/// A maximized chat window: its frame turns white when the window is activated; repaint it flat after every frame paint.
unsafe extern "system" fn child_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    // A chat window that is not maximized has its own title bar and frame: they are drawn here in the theme's
    // colours instead of the system's blue (or grey) caption.
    if !IsZoomed(h).as_bool() {
        match msg {
            WM_NCPAINT => {
                paint_caption_frame(h);
                return windows::Win32::Foundation::LRESULT(0);
            }
            WM_NCACTIVATE => {
                let _ = SetPropW(h, w!("chimera.active"), windows::Win32::Foundation::HANDLE(wp.0 as *mut c_void));
                // lParam -1: the system does the activation but does not repaint the frame itself
                let r = windows::Win32::UI::Shell::DefSubclassProc(h, msg, wp, LPARAM(-1));
                paint_caption_frame(h);
                return r;
            }
            WM_SETTEXT | WM_SETICON => {
                let r = windows::Win32::UI::Shell::DefSubclassProc(h, msg, wp, lp);
                paint_caption_frame(h);
                return r;
            }
            _ => {}
        }
    }
    let r = windows::Win32::UI::Shell::DefSubclassProc(h, msg, wp, lp);
    let t = theme();
    if (msg == WM_NCPAINT || msg == WM_NCACTIVATE) && IsZoomed(h).as_bool() {
        let mut win = RECT::default();
        let mut cl = RECT::default();
        let mut origin = POINT { x: 0, y: 0 };
        if GetWindowRect(h, &mut win).is_ok() && GetClientRect(h, &mut cl).is_ok() && ClientToScreen(h, &mut origin).as_bool() {
            let (w, ht) = (win.right - win.left, win.bottom - win.top);
            let (l, tp) = (origin.x - win.left, origin.y - win.top);
            let client = RECT { left: l, top: tp, right: l + cl.right, bottom: tp + cl.bottom };
            let dc = GetWindowDC(h);
            if !dc.is_invalid() && w > 4 && ht > 4 {
                ExcludeClipRect(dc, client.left, client.top, client.right, client.bottom);
                let whole = RECT { left: 0, top: 0, right: w, bottom: ht };
                FillRect(dc, &whole, panel_brush());
                let line = CreateSolidBrush(colorref(t.border));
                FrameRect(dc, &RECT { left: l - 1, top: tp - 1, right: client.right + 1, bottom: client.bottom + 1 }, line);
                let _ = DeleteObject(HGDIOBJ(line.0));
                ReleaseDC(h, dc);
            }
        }
    }
    r
}

/// Status bar in the dark theme: parts drawn through the owner, framed flat (the control's own frames are white).
unsafe extern "system" fn status_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{LRESULT, RECT};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    if msg == WM_PAINT {
        let parent = GetParent(h).unwrap_or_default();
        let id = GetDlgCtrlID(h) as usize;
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(h, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(h, &mut client);
        FillRect(dc, &client, panel_brush());
        let count = SendMessageW(h, 0x406 /* SB_GETPARTS */, WPARAM(0), LPARAM(0)).0.max(0) as usize;
        let line = CreateSolidBrush(colorref(theme().border));
        for i in 0..count.min(3) {
            let mut rc = RECT::default();
            if SendMessageW(h, 0x40A /* SB_GETRECT */, WPARAM(i), LPARAM(&mut rc as *mut RECT as isize)).0 == 0 {
                continue;
            }
            let dis = DRAWITEMSTRUCT {
                CtlType: ODT_STATIC,
                CtlID: id as u32,
                itemID: i as u32,
                itemAction: ODA_DRAWENTIRE,
                itemState: ODS_DEFAULT,
                hwndItem: h,
                hDC: dc,
                rcItem: rc,
                itemData: 0,
            };
            SendMessageW(parent, WM_DRAWITEM, WPARAM(id), LPARAM(&dis as *const DRAWITEMSTRUCT as isize));
            FrameRect(dc, &rc, line);
        }
        let _ = DeleteObject(HGDIOBJ(line.0));
        let _ = EndPaint(h, &ps);
        return LRESULT(0);
    }
    windows::Win32::UI::Shell::DefSubclassProc(h, msg, wp, lp)
}

/// Repaint the two-pixel sunken edge of a box flat: a line in the border colour and one in the box's own colour.
unsafe extern "system" fn edge_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let r = windows::Win32::UI::Shell::DefSubclassProc(h, msg, wp, lp);
    let t = theme();
    if msg == WM_NCPAINT && (GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_CLIENTEDGE.0) != 0 {
        let mut rc = RECT::default();
        if GetWindowRect(h, &mut rc).is_ok() {
            let (w, ht) = (rc.right - rc.left, rc.bottom - rc.top);
            let dc = GetWindowDC(h);
            if !dc.is_invalid() && w > 4 && ht > 4 {
                let outer = CreateSolidBrush(colorref(t.border));
                let inner = CreateSolidBrush(colorref(t.bg));
                FrameRect(dc, &RECT { left: 0, top: 0, right: w, bottom: ht }, outer);
                FrameRect(dc, &RECT { left: 1, top: 1, right: w - 1, bottom: ht - 1 }, inner);
                let _ = DeleteObject(HGDIOBJ(outer.0));
                let _ = DeleteObject(HGDIOBJ(inner.0));
                ReleaseDC(h, dc);
            }
        }
    }
    r
}

/// Column headers: the system draws them with black text whatever the theme, so in the dark theme they are painted here.
unsafe extern "system" fn header_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{LRESULT, RECT};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let t = theme();
    if msg == WM_PAINT || msg == WM_ERASEBKGND {
        if msg == WM_ERASEBKGND {
            return LRESULT(1);
        }
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(h, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(h, &mut client);
        FillRect(dc, &client, panel_brush());
        let font = HFONT(SendMessageW(h, WM_GETFONT, WPARAM(0), LPARAM(0)).0 as *mut c_void);
        let old = SelectObject(dc, font);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, colorref(t.panel_text));
        let line = CreatePen(PS_SOLID, 1, colorref(t.timestamp));
        let oldpen = SelectObject(dc, line);
        let count = SendMessageW(h, 0x1200 /* HDM_GETITEMCOUNT */, WPARAM(0), LPARAM(0)).0.max(0) as usize;
        for i in 0..count {
            let mut rc = RECT::default();
            if SendMessageW(h, 0x1207 /* HDM_GETITEMRECT */, WPARAM(i), LPARAM(&mut rc as *mut RECT as isize)).0 == 0 {
                continue;
            }
            let mut text = [0u16; 128];
            // HDITEMW: mask, cxy, pszText, hbm, cchTextMax ... (only the text is wanted)
            let mut item = windows::Win32::UI::Controls::HDITEMW::default();
            item.mask = windows::Win32::UI::Controls::HDI_TEXT;
            item.pszText = windows::core::PWSTR(text.as_mut_ptr());
            item.cchTextMax = 127;
            SendMessageW(h, 0x120B /* HDM_GETITEMW */, WPARAM(i), LPARAM(&mut item as *mut _ as isize));
            let n = text.iter().position(|&c| c == 0).unwrap_or(0);
            let mut tr = RECT { left: rc.left + 6, top: rc.top, right: rc.right - 4, bottom: rc.bottom };
            if n > 0 && tr.right > tr.left && tr.bottom > tr.top {
                text[n] = 0;
                DrawTextW(dc, &mut text[..n], &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
            }
            let _ = MoveToEx(dc, rc.right - 1, rc.top + 2, None);
            let _ = LineTo(dc, rc.right - 1, rc.bottom - 2);
        }
        SelectObject(dc, oldpen);
        let _ = DeleteObject(line);
        SelectObject(dc, old);
        let _ = EndPaint(h, &ps);
        return LRESULT(0);
    }
    windows::Win32::UI::Shell::DefSubclassProc(h, msg, wp, lp)
}

/// Style a window and everything in it.
pub fn style_tree(root: HWND) {
    if root.0.is_null() {
        return;
    }
    let class = class_of(root);
    if class == "ChimeraIRC.Form" || class == "#32770" {
        return; // dialogs keep the system look
    }
    style_one(root);
    if class.starts_with("ChimeraIRC.") {
        style_title_bar(root);
    }
    extern "system" fn each(h: HWND, _: LPARAM) -> BOOL {
        style_one(h);
        true.into()
    }
    unsafe {
        let _ = EnumChildWindows(root, Some(each), LPARAM(0));
    }
}

// ---------------------------------------------------------------- dark pop-up menus

/// Windows' own pop-up menus (and the system menu) follow the dark theme when the app asks for it. This uses
/// the undocumented ordinals 135 (SetPreferredAppMode) and 136 (FlushMenuThemes) of uxtheme.dll, which Windows
/// 10 1903 and later have; on anything else it does nothing.
fn prefer_dark_popups(dark: bool) {
    unsafe {
        let Ok(lib) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        let set = GetProcAddress(lib, PCSTR_ordinal(135));
        let flush = GetProcAddress(lib, PCSTR_ordinal(136));
        if let Some(set) = set {
            let set: extern "system" fn(i32) -> i32 = std::mem::transmute(set);
            // 2 = force dark, 3 = force light
            set(if dark { 2 } else { 3 });
        }
        if let Some(flush) = flush {
            let flush: extern "system" fn() = std::mem::transmute(flush);
            flush();
        }
    }
}

#[allow(non_snake_case)]
fn PCSTR_ordinal(n: u16) -> windows::core::PCSTR {
    windows::core::PCSTR(n as usize as *const u8)
}

// ---------------------------------------------------------------- the font chooser

/// Ask for a font with the standard font dialog; `None` when cancelled.
pub fn pick_font(owner: HWND, current: &FontSpec) -> Option<FontSpec> {
    unsafe {
        let dpi = GetDpiForSystem() as i32;
        let mut lf = LOGFONTW { lfHeight: -(current.points as i32 * dpi.max(96) / 72), lfWeight: if current.bold { 700 } else { 400 }, ..Default::default() };
        for (i, c) in current.face.encode_utf16().take(31).enumerate() {
            lf.lfFaceName[i] = c;
        }
        let mut cf = CHOOSEFONTW {
            lStructSize: std::mem::size_of::<CHOOSEFONTW>() as u32,
            hwndOwner: owner,
            lpLogFont: &mut lf,
            Flags: CF_SCREENFONTS | CF_INITTOLOGFONTSTRUCT | CF_LIMITSIZE,
            nSizeMin: crate::theme::MIN_POINTS as i32,
            nSizeMax: crate::theme::MAX_POINTS as i32,
            ..Default::default()
        };
        if !ChooseFontW(&mut cf).as_bool() {
            return None;
        }
        let n = lf.lfFaceName.iter().position(|c| *c == 0).unwrap_or(32);
        let face = String::from_utf16_lossy(&lf.lfFaceName[..n]);
        let points = ((cf.iPointSize + 5) / 10).max(0) as u32;
        Some(FontSpec::new(&face, points, lf.lfWeight >= 700))
    }
}

// ---------------------------------------------------------------- the dark menu bar

const WM_UAHDRAWMENU: u32 = 0x0091;
const WM_UAHDRAWMENUITEM: u32 = 0x0092;

/// Windows asks the window to draw its menu bar with two undocumented messages; in the dark theme they are
/// answered here (in the light theme `None` is returned and the system draws the bar as usual).
pub unsafe fn menu_bar_message(hwnd: HWND, msg: u32, lp: LPARAM) -> Option<windows::Win32::Foundation::LRESULT> {
    let t = theme();
    match msg {
        WM_UAHDRAWMENU => {
            // lParam -> { HMENU hmenu; HDC hdc; DWORD flags }
            let hdc = HDC(*((lp.0 as *const u8).add(8) as *const isize) as *mut c_void);
            let mut mbi = MENUBARINFO { cbSize: std::mem::size_of::<MENUBARINFO>() as u32, ..Default::default() };
            if GetMenuBarInfo(hwnd, OBJID_MENU, 0, &mut mbi).is_err() {
                return None;
            }
            let mut wr = windows::Win32::Foundation::RECT::default();
            let _ = GetWindowRect(hwnd, &mut wr);
            let mut rc = mbi.rcBar;
            let _ = OffsetRect(&mut rc, -wr.left, -wr.top);
            FillRect(hdc, &rc, panel_brush());
            Some(windows::Win32::Foundation::LRESULT(1))
        }
        WM_UAHDRAWMENUITEM => {
            // lParam -> { DRAWITEMSTRUCT dis; UAHMENU um; UAHMENUITEM umi }: the menu is at 64, the position at 88
            let dis = &*(lp.0 as *const DRAWITEMSTRUCT);
            let hmenu = HMENU(*((lp.0 as *const u8).add(64) as *const isize) as *mut c_void);
            let pos = *((lp.0 as *const u8).add(88) as *const i32);
            let mut buf = [0u16; 80];
            let mut mii = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_STRING | MIIM_BITMAP,
                dwTypeData: windows::core::PWSTR(buf.as_mut_ptr()),
                cch: 79,
                ..Default::default()
            };
            let _ = GetMenuItemInfoW(hmenu, pos as u32, true, &mut mii);
            let hot = dis.itemState.0 & (ODS_HOTLIGHT.0 | ODS_SELECTED.0) != 0;
            let bg = if hot { Rgb(t.panel.0.saturating_add(0x1c), t.panel.1.saturating_add(0x1c), t.panel.2.saturating_add(0x1c)) } else { t.panel };
            let brush = CreateSolidBrush(colorref(bg));
            FillRect(dis.hDC, &dis.rcItem, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
            SetBkMode(dis.hDC, TRANSPARENT);
            SetTextColor(dis.hDC, colorref(t.panel_text));
            let mut rc = dis.rcItem;
            // A maximized chat window puts its system-menu icon first in the bar and minimize / restore / close
            // buttons last: they are bitmaps the system knows (HBMMENU_*), so they are drawn here by hand.
            let special = mii.hbmpItem.0 as isize;
            if (1..=7).contains(&special) {
                let w = rc.right - rc.left;
                let h = rc.bottom - rc.top;
                if special == 1 {
                    let mdi = windows::Win32::UI::WindowsAndMessaging::FindWindowExW(hwnd, None, w!("MDIClient"), PCWSTR::null()).unwrap_or_default();
                    let active = HWND(SendMessageW(mdi, 0x0229, WPARAM(0), LPARAM(0)).0 as *mut c_void);
                    let mut icon = HICON(SendMessageW(active, WM_GETICON, WPARAM(ICON_SMALL as usize), LPARAM(0)).0 as *mut c_void);
                    if icon.0.is_null() {
                        icon = HICON(GetClassLongPtrW(active, GCLP_HICONSM) as *mut c_void);
                    }
                    if icon.0.is_null() {
                        icon = HICON(SendMessageW(hwnd, WM_GETICON, WPARAM(ICON_SMALL as usize), LPARAM(0)).0 as *mut c_void);
                    }
                    if !icon.0.is_null() {
                        let side = (h - 6).clamp(12, 20);
                        let _ = DrawIconEx(dis.hDC, rc.left + (w - side) / 2, rc.top + (h - side) / 2, icon, side, side, 0, None, DI_NORMAL);
                    }
                } else {
                    // Marlett is the font the system draws caption buttons with: 0 minimize, 2 restore, r close
                    let (glyph, enabled) = match special {
                        2 => ('2', true),
                        3 => ('0', true),
                        7 => ('0', false),
                        6 => ('r', false),
                        _ => ('r', true),
                    };
                    let face = crate::util::wide("Marlett");
                    let font = CreateFontW(-(h * 6 / 10).max(8), 0, 0, 0, 400, 0, 0, 0, 2 /* SYMBOL_CHARSET */, 0, 0, 0, 0, PCWSTR(face.as_ptr()));
                    let old = SelectObject(dis.hDC, HGDIOBJ(font.0));
                    SetTextColor(dis.hDC, colorref(if enabled { t.panel_text } else { t.timestamp }));
                    let mut g = [glyph as u16];
                    DrawTextW(dis.hDC, &mut g, &mut rc, DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
                    SelectObject(dis.hDC, old);
                    let _ = DeleteObject(HGDIOBJ(font.0));
                }
                return Some(windows::Win32::Foundation::LRESULT(1));
            }
            let n = buf.iter().position(|c| *c == 0).unwrap_or(0);
            let flags = DT_CENTER | DT_SINGLELINE | DT_VCENTER | if dis.itemState.0 & ODS_NOACCEL.0 != 0 { DT_HIDEPREFIX } else { DRAW_TEXT_FORMAT(0) };
            if n > 0 && rc.right > rc.left && rc.bottom > rc.top {
                DrawTextW(dis.hDC, &mut buf[..n], &mut rc, flags);
            }
            Some(windows::Win32::Foundation::LRESULT(1))
        }
        _ => None,
    }
}

/// `WM_UAHMEASUREMENUITEM` (0x94): the lParam is { MEASUREITEMSTRUCT mis; UAHMENU um; UAHMENUITEM umi }. When the item
/// is the system-menu icon that a maximized chat window adds to the bar, its width is set to nothing: the
/// minimize / restore / close buttons at the other end say the same.
pub unsafe fn hide_system_icon_width(lp: LPARAM) {
    if lp.0 == 0 {
        return;
    }
    let base = lp.0 as *mut u8;
    let hmenu = HMENU(*(base.add(32) as *const isize) as *mut c_void);
    let pos = *(base.add(56) as *const i32);
    if hmenu.0.is_null() || pos < 0 {
        return;
    }
    let mut mii = MENUITEMINFOW { cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32, fMask: MIIM_BITMAP, ..Default::default() };
    if GetMenuItemInfoW(hmenu, pos as u32, true, &mut mii).is_ok() && mii.hbmpItem.0 as isize == 1 {
        *(base.add(12) as *mut u32) = 0; // MEASUREITEMSTRUCT.itemWidth
    }
}

/// The menu bar has a one pixel line under it that the system draws in its own colour; paint it over.
pub unsafe fn paint_menu_bar_line(hwnd: HWND) {
    let mut mbi = MENUBARINFO { cbSize: std::mem::size_of::<MENUBARINFO>() as u32, ..Default::default() };
    if GetMenuBarInfo(hwnd, OBJID_MENU, 0, &mut mbi).is_err() {
        return;
    }
    let mut wr = windows::Win32::Foundation::RECT::default();
    let _ = GetWindowRect(hwnd, &mut wr);
    let mut rc = mbi.rcBar;
    let _ = OffsetRect(&mut rc, -wr.left, -wr.top);
    rc.top = rc.bottom;
    rc.bottom += 1;
    let dc = GetWindowDC(hwnd);
    FillRect(dc, &rc, panel_brush());
    ReleaseDC(hwnd, dc);
}
