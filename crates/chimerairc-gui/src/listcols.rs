//! Reading and applying the column widths of Win32 list views (in 96-dpi pixels).

use crate::util::px;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Controls::{LVM_GETCOLUMNWIDTH, LVM_SETCOLUMNWIDTH};
use windows::Win32::UI::WindowsAndMessaging::SendMessageW;

/// The widths of the first `n` columns of `list`.
pub fn read(list: HWND, n: usize) -> Vec<u32> {
    let dev96 = px(96).max(1);
    (0..n)
        .map(|i| {
            let w = unsafe { SendMessageW(list, LVM_GETCOLUMNWIDTH, WPARAM(i), LPARAM(0)) }.0 as i32;
            (w.max(0) * 96 / dev96) as u32
        })
        .collect()
}

/// Set the widths of the columns of `list`.
pub fn apply(list: HWND, widths: &[u32]) {
    for (i, w) in widths.iter().enumerate() {
        unsafe {
            SendMessageW(list, LVM_SETCOLUMNWIDTH, WPARAM(i), LPARAM(px(*w as i32) as isize));
        }
    }
}
