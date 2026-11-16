//! Help > About ChimeraIRC: the logo, the version (from the build), a short description, the licence and a link to
//! the project. A small modal window of plain controls, like the other dialogs.

use crate::ui;
use crate::util::*;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("ChimeraIRC.About");
static REGISTERED: std::sync::Once = std::sync::Once::new();
static DONE: AtomicBool = AtomicBool::new(false);

/// What the program is: shown in the window and copied by the button (handy in a bug report).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const AUTHOR: &str = "by Howard E. DeWitt";
pub const DESCRIPTION: &str = "A Windows IRC client written in Rust, in the spirit of HydraIRC";
pub const LICENCE: &str = "Free software, licensed under the GNU General Public License, version 3 or later.";
pub const HOME: &str = "https://github.com/theG33K/chimerairc";

const ID_COPY: usize = 201;
const ID_LINK: usize = 202;

/// The text the "Copy version" button puts on the clipboard.
pub fn version_text() -> String {
    let arch = if cfg!(target_arch = "x86_64") { "x64" } else if cfg!(target_arch = "aarch64") { "ARM64" } else { "x86" };
    let build = if cfg!(debug_assertions) { "debug build" } else { "release build" };
    format!("ChimeraIRC {VERSION} (Windows {arch}, {build})")
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            // the big icon (the 128 pixel picture of the executable's icon)
            let side = px(128);
            let icon = LoadImageW(ui::hinstance(), PCWSTR(1usize as *const u16), IMAGE_ICON, side, side, LR_DEFAULTCOLOR);
            if let Ok(h) = icon {
                let icon = HICON(h.0);
                let _ = DrawIconEx(dc, px(22), px(22), icon, side, side, 0, None, DI_NORMAL);
                let _ = DestroyIcon(icon);
            }
            // a line above the buttons
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let line = RECT { left: 0, top: rc.bottom - px(52), right: rc.right, bottom: rc.bottom - px(52) + 1 };
            FillRect(dc, &line, GetSysColorBrush(COLOR_3DLIGHT));
            let below = RECT { left: 0, top: line.bottom, right: rc.right, bottom: rc.bottom };
            FillRect(dc, &below, GetSysColorBrush(COLOR_BTNFACE));
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            // labels and the link sit on the white area
            let dc = HDC(wp.0 as *mut c_void);
            SetBkMode(dc, TRANSPARENT);
            LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
        }
        WM_COMMAND => {
            let id = wp.0 & 0xffff;
            if id == ID_COPY {
                ui::copy_to_clipboard(&version_text());
            } else if id == 1 || id == 2 {
                DONE.store(true, Ordering::Relaxed); // OK, Enter or Escape
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            let nm = &*(lp.0 as *const NMHDR);
            if nm.idFrom == ID_LINK && (nm.code == NM_CLICK || nm.code == NM_RETURN) {
                let link = &*(lp.0 as *const NMLINK);
                let n = link.item.szUrl.iter().position(|c| *c == 0).unwrap_or(0);
                let url = String::from_utf16_lossy(&link.item.szUrl[..n]);
                // only the project's own web address is ever opened from here
                if url.starts_with("https://") {
                    let u = wide(&url);
                    ShellExecuteW(None, w!("open"), PCWSTR(u.as_ptr()), None, None, SW_SHOWNORMAL);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            DONE.store(true, Ordering::Relaxed);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Show the About window over `owner` and wait until it is closed.
pub fn show(owner: HWND) {
    unsafe {
        let hi = ui::hinstance();
        REGISTERED.call_once(|| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: hi,
                lpszClassName: CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hIcon: LoadIconW(hi, PCWSTR(1usize as *const u16)).unwrap_or_default(),
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                ..Default::default()
            };
            RegisterClassW(&wc);
        });

        let (client_w, client_h) = (px(470), px(296));
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let ex = WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT;
        let mut rc = RECT { left: 0, top: 0, right: client_w, bottom: client_h };
        let _ = AdjustWindowRectEx(&mut rc, style, false, ex);
        let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
        let mut orc = RECT::default();
        let _ = GetWindowRect(owner, &mut orc);
        let x = (orc.left + (orc.right - orc.left - ww) / 2).max(0);
        let y = (orc.top + (orc.bottom - orc.top - wh) / 3).max(0);
        let title = wide("About ChimeraIRC");
        let Ok(dlg) = CreateWindowExW(ex, CLASS, PCWSTR(title.as_ptr()), style, x, y, ww, wh, owner, None, hi, None) else { return };

        let font = ui::ui_font_raw();
        let big = CreateFontW(-px(26), 0, 0, 0, 700, 0, 0, 0, 1, 0, 0, 5, 0, w!("Segoe UI"));
        let mk = |class: PCWSTR, text: &str, style: WINDOW_STYLE, x: i32, y: i32, w: i32, h: i32, id: usize, f: isize| {
            let t = wide(text);
            let c = CreateWindowExW(
                WINDOW_EX_STYLE(0), class, PCWSTR(t.as_ptr()), WS_CHILD | WS_VISIBLE | style, x, y, w, h, dlg,
                HMENU(id as *mut c_void), hi, None,
            )
            .unwrap_or_default();
            SendMessageW(c, WM_SETFONT, WPARAM(f as usize), LPARAM(1));
            c
        };
        let left = px(170);
        let col = client_w - left - px(20);
        mk(w!("STATIC"), "ChimeraIRC", WINDOW_STYLE(0), left, px(22), col, px(36), 0, big.0 as isize);
        mk(w!("STATIC"), AUTHOR, WINDOW_STYLE(0), left, px(58), col, px(20), 0, font);
        mk(w!("STATIC"), &format!("Version {VERSION}"), WINDOW_STYLE(0), left, px(80), col, px(20), 0, font);
        mk(w!("STATIC"), DESCRIPTION, WINDOW_STYLE(0), left, px(110), col, px(40), 0, font);
        mk(w!("STATIC"), LICENCE, WINDOW_STYLE(0), left, px(154), col, px(40), 0, font);
        let link_text = format!("<a href=\"{HOME}\">{HOME}</a>");
        mk(WC_LINK, &link_text, WS_TABSTOP, left, px(200), col, px(22), ID_LINK, font);
        // (the line above the buttons is 52 pixels from the bottom: the link ends well above it)
        let by = client_h - px(40);
        mk(w!("BUTTON"), "Copy version", WS_TABSTOP, client_w - px(20) - px(90) - px(8) - px(110), by, px(110), px(28), ID_COPY, font);
        let ok = mk(w!("BUTTON"), "OK", WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32), client_w - px(20) - px(90), by, px(90), px(28), 1, font);

        DONE.store(false, Ordering::Relaxed);
        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetFocus(ok);
        let mut m = MSG::default();
        while !DONE.load(Ordering::Relaxed) {
            if !GetMessageW(&mut m, None, 0, 0).as_bool() {
                break;
            }
            if !IsDialogMessageW(dlg, &m).as_bool() {
                let _ = TranslateMessage(&m);
                DispatchMessageW(&m);
            }
        }
        let _ = EnableWindow(owner, true);
        let _ = DestroyWindow(dlg);
        let _ = DeleteObject(HGDIOBJ(big.0));
        let _ = SetForegroundWindow(owner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_is_what_was_asked_for_and_the_version_comes_from_the_build() {
        assert_eq!(DESCRIPTION, "A Windows IRC client written in Rust, in the spirit of HydraIRC");
        assert_eq!(AUTHOR, "by Howard E. DeWitt");
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        let v = version_text();
        assert!(v.starts_with(&format!("ChimeraIRC {VERSION} (Windows ")) && v.ends_with("build)"), "{v}");
        assert!(HOME.starts_with("https://"), "the link opens only https addresses");
    }
}
