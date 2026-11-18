//! A small modal "form" dialog built from plain controls (no .rc dialog templates):
//! labelled text / password / number / checkbox rows plus OK and Cancel.

use crate::ui;
use crate::util::*;
use std::ffi::c_void;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Password,
    Number,
    Check,
    /// A checkbox that spans the whole row (for long labels).
    CheckWide,
    /// A label with a tall, scrolling, multi-line edit box below it.
    Multiline,
    /// Explanatory text across the whole row; it has no value.
    Note,
}

impl FieldKind {
    fn is_check(self) -> bool {
        matches!(self, FieldKind::Check | FieldKind::CheckWide)
    }
}

pub struct Field {
    pub label: String,
    pub kind: FieldKind,
    /// Initial value; for checkboxes, "1" means checked.
    pub value: String,
}

impl Field {
    pub fn text(label: &str, value: &str) -> Field {
        Field { label: label.into(), kind: FieldKind::Text, value: value.into() }
    }
    pub fn password(label: &str) -> Field {
        Field { label: label.into(), kind: FieldKind::Password, value: String::new() }
    }
    pub fn number(label: &str, value: &str) -> Field {
        Field { label: label.into(), kind: FieldKind::Number, value: value.into() }
    }
    pub fn check(label: &str, on: bool) -> Field {
        Field { label: label.into(), kind: FieldKind::Check, value: if on { "1" } else { "0" }.into() }
    }
    pub fn multiline(label: &str, value: &str) -> Field {
        Field { label: label.into(), kind: FieldKind::Multiline, value: value.into() }
    }
    pub fn note(text: &str) -> Field {
        Field { label: text.into(), kind: FieldKind::Note, value: String::new() }
    }
    pub fn wide_check(label: &str, on: bool) -> Field {
        Field { label: label.into(), kind: FieldKind::CheckWide, value: if on { "1" } else { "0" }.into() }
    }
}

struct State {
    controls: Vec<HWND>,
    kinds: Vec<FieldKind>,
    result: Option<Vec<String>>,
    done: bool,
}

pub(crate) const ID_OK: usize = 1;
pub(crate) const ID_CANCEL: usize = 2;
const CLASS: PCWSTR = w!("ChimeraIRC.Form");

static REGISTERED: std::sync::Once = std::sync::Once::new();

fn read(h: HWND, kind: FieldKind) -> String {
    unsafe {
        if kind == FieldKind::Note {
            return String::new();
        }
        if kind.is_check() {
            let on = SendMessageW(h, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == BST_CHECKED.0 as isize;
            return if on { "1" } else { "0" }.into();
        }
        let n = GetWindowTextLengthW(h) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(h, &mut buf) as usize;
        String::from_utf16_lossy(&buf[..got])
    }
}

extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        match msg {
            WM_COMMAND if !st.is_null() => {
                match wp.0 & 0xffff {
                    ID_OK => {
                        let s = &mut *st;
                        s.result = Some(s.controls.iter().zip(&s.kinds).map(|(h, k)| read(*h, *k)).collect());
                        s.done = true;
                    }
                    ID_CANCEL => (*st).done = true,
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

/// Show the form modally over `owner`. Returns the values on OK, `None` on Cancel/close.
pub fn show(owner: HWND, title: &str, fields: &[Field]) -> Option<Vec<String>> {
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

        let (margin, label_w, ctrl_w, row_h, edit_h) = (px(14), px(120), px(250), px(30), px(24));
        let client_w = margin * 2 + label_w + px(8) + ctrl_w;
        let full_w = label_w + px(8) + ctrl_w;
        let multiline_h = px(150);
        let height_of = |k: FieldKind| match k {
            FieldKind::Multiline => edit_h + multiline_h + px(6),
            FieldKind::Note => px(112),
            _ => row_h,
        };
        let total: i32 = fields.iter().map(|f| height_of(f.kind)).sum();
        let client_h = margin + total + px(8) + px(26) + margin;
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let ex = WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT;
        let mut rc = RECT { left: 0, top: 0, right: client_w, bottom: client_h };
        let _ = AdjustWindowRectEx(&mut rc, style, false, ex);
        let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
        let mut orc = RECT::default();
        let _ = GetWindowRect(owner, &mut orc);
        let x = (orc.left + (orc.right - orc.left - ww) / 2).max(0);
        let y = (orc.top + (orc.bottom - orc.top - wh) / 3).max(0);

        let t = wide(title);
        let dlg = CreateWindowExW(ex, CLASS, PCWSTR(t.as_ptr()), style, x, y, ww, wh, owner, None, hi, None).ok()?;
        let font = ui::ui_font_raw();
        let mut state = Box::new(State { controls: vec![], kinds: vec![], result: None, done: false });

        let mut first_edit = HWND::default();
        for (i, f) in fields.iter().enumerate() {
            let top = margin + fields[..i].iter().map(|f| height_of(f.kind)).sum::<i32>();
            let mk = |class: PCWSTR, text: &str, style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, x, y, w, h| {
                let t = wide(text);
                let h = CreateWindowExW(ex, class, PCWSTR(t.as_ptr()), WS_CHILD | WS_VISIBLE | style, x, y, w, h, dlg, None, hi, None)
                    .unwrap_or_default();
                SendMessageW(h, WM_SETFONT, WPARAM(font as usize), LPARAM(1));
                h
            };
            let ctrl = match f.kind {
                FieldKind::CheckWide => mk(
                    w!("BUTTON"), &f.label, WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32), WINDOW_EX_STYLE(0),
                    margin, top, label_w + px(8) + ctrl_w, edit_h,
                ),
                FieldKind::Check => mk(
                    w!("BUTTON"), &f.label, WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32), WINDOW_EX_STYLE(0),
                    margin + label_w + px(8), top, ctrl_w, edit_h,
                ),
                FieldKind::Note => mk(w!("STATIC"), &f.label, WINDOW_STYLE(0), WINDOW_EX_STYLE(0), margin, top, full_w, px(106)),
                FieldKind::Multiline => {
                    mk(w!("STATIC"), &f.label, WINDOW_STYLE(0x200), WINDOW_EX_STYLE(0), margin, top, full_w, edit_h);
                    mk(
                        w!("EDIT"),
                        &f.value.replace("\r\n", "\n").replace('\n', "\r\n"),
                        WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32),
                        WS_EX_CLIENTEDGE,
                        margin, top + edit_h, full_w, multiline_h,
                    )
                }
                kind => {
                    mk(w!("STATIC"), &f.label, WINDOW_STYLE(0x200) /* SS_CENTERIMAGE */, WINDOW_EX_STYLE(0), margin, top, label_w, edit_h);
                    let extra = match kind {
                        FieldKind::Password => ES_PASSWORD as u32,
                        FieldKind::Number => ES_NUMBER as u32,
                        _ => 0,
                    };
                    mk(
                        w!("EDIT"), &f.value, WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32 | extra), WS_EX_CLIENTEDGE,
                        margin + label_w + px(8), top, ctrl_w, edit_h,
                    )
                }
            };
            if f.kind.is_check() && f.value == "1" {
                SendMessageW(ctrl, BM_SETCHECK, WPARAM(BST_CHECKED.0 as usize), LPARAM(0));
            }
            if first_edit.0.is_null() && !f.kind.is_check() && f.kind != FieldKind::Note {
                first_edit = ctrl;
            }
            state.controls.push(ctrl);
            state.kinds.push(f.kind);
        }

        let by = client_h - margin - px(26);
        let bw = px(84);
        let button = |text: &str, id: usize, x: i32, default: bool| {
            let t = wide(text);
            let st = WS_CHILD | WS_VISIBLE | WS_TABSTOP
                | WINDOW_STYLE(if default { BS_DEFPUSHBUTTON } else { BS_PUSHBUTTON } as u32);
            let h = CreateWindowExW(
                WINDOW_EX_STYLE(0), w!("BUTTON"), PCWSTR(t.as_ptr()), st, x, by, bw, px(26), dlg,
                HMENU(id as *mut c_void), hi, None,
            )
            .unwrap_or_default();
            SendMessageW(h, WM_SETFONT, WPARAM(font as usize), LPARAM(1));
        };
        button("OK", ID_OK, client_w - margin - bw * 2 - px(8), true);
        button("Cancel", ID_CANCEL, client_w - margin - bw, false);

        SetWindowLongPtrW(dlg, GWLP_USERDATA, &mut *state as *mut State as isize);
        crate::look::style_popup(dlg);
        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(dlg, SW_SHOW);
        if !first_edit.0.is_null() {
            let _ = SetFocus(first_edit);
            SendMessageW(first_edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
        }

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

pub fn message(owner: HWND, title: &str, text: &str) {
    let (t, x) = (wide(title), wide(text));
    unsafe {
        MessageBoxW(owner, PCWSTR(x.as_ptr()), PCWSTR(t.as_ptr()), MB_OK | MB_ICONWARNING);
    }
}

/// Plain information box (no warning icon).
pub fn info(owner: HWND, title: &str, text: &str) {
    let (t, x) = (wide(title), wide(text));
    unsafe {
        MessageBoxW(owner, PCWSTR(x.as_ptr()), PCWSTR(t.as_ptr()), MB_OK | MB_ICONINFORMATION);
    }
}

/// The standard "open a file" dialog. `None` if cancelled or unavailable.
pub fn pick_open_file(owner: HWND, title: &str) -> Option<std::path::PathBuf> {
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, SIGDN_FILESYSPATH};
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg.GetOptions().ok()?;
        dlg.SetOptions(opts | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM).ok()?;
        let t = wide(title);
        dlg.SetTitle(PCWSTR(t.as_ptr())).ok()?;
        dlg.Show(owner).ok()?;
        let name = dlg.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const c_void));
        path.map(std::path::PathBuf::from)
    }
}

/// The standard "save as" dialog, starting in the Downloads folder with `default_name` filled in
/// and an overwrite prompt. `None` if cancelled or unavailable.
pub fn pick_save_file(owner: HWND, default_name: &str, title: &str) -> Option<std::path::PathBuf> {
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{
        FileSaveDialog, FOLDERID_Downloads, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SHGetKnownFolderPath,
        FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, KF_FLAG_DEFAULT, SIGDN_FILESYSPATH,
    };
    unsafe {
        let dlg: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg.GetOptions().ok()?;
        dlg.SetOptions(opts | FOS_OVERWRITEPROMPT | FOS_FORCEFILESYSTEM).ok()?;
        let t = wide(title);
        dlg.SetTitle(PCWSTR(t.as_ptr())).ok()?;
        let n = wide(default_name);
        dlg.SetFileName(PCWSTR(n.as_ptr())).ok()?;
        // Best effort: open in Downloads rather than wherever the dialog last was.
        if let Ok(dl) = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None) {
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(dl.0), None) {
                let _ = dlg.SetFolder(&item);
            }
            CoTaskMemFree(Some(dl.0 as *const c_void));
        }
        dlg.Show(owner).ok()?;
        let name = dlg.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const c_void));
        path.map(std::path::PathBuf::from)
    }
}

/// Yes/No warning. Defaults to No.
/// Yes / No / Cancel question (the default is Cancel). `Some(true)` = Yes, `Some(false)` = No.
pub fn ask3(owner: HWND, title: &str, text: &str) -> Option<bool> {
    let (t, x) = (wide(title), wide(text));
    let r = unsafe {
        MessageBoxW(owner, PCWSTR(x.as_ptr()), PCWSTR(t.as_ptr()), MB_YESNOCANCEL | MB_ICONQUESTION | MB_DEFBUTTON3)
    };
    if r == IDYES {
        Some(true)
    } else if r == IDNO {
        Some(false)
    } else {
        None
    }
}

pub fn confirm(owner: HWND, title: &str, text: &str) -> bool {
    let (t, x) = (wide(title), wide(text));
    unsafe {
        MessageBoxW(owner, PCWSTR(x.as_ptr()), PCWSTR(t.as_ptr()), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) == IDYES
    }
}
