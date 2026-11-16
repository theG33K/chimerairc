use std::ffi::c_void;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::GetDpiForSystem;

/// NUL-terminated UTF-16.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn hw(v: isize) -> HWND {
    HWND(v as *mut c_void)
}

pub fn raw(h: HWND) -> isize {
    h.0 as isize
}

/// The IPv4 default gateway (the router): the next hop of the best default route, if there is one.
pub fn default_gateway() -> Option<std::net::Ipv4Addr> {
    use windows::Win32::NetworkManagement::IpHelper::{GetIpForwardTable, MIB_IPFORWARDTABLE};
    unsafe {
        let mut size = 0u32;
        let _ = GetIpForwardTable(None, &mut size, false); // asks for the needed size
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let table = buf.as_mut_ptr() as *mut MIB_IPFORWARDTABLE;
        if GetIpForwardTable(Some(table), &mut size, false) != 0 {
            return None;
        }
        let rows = std::slice::from_raw_parts((*table).table.as_ptr(), (*table).dwNumEntries as usize);
        rows.iter()
            .filter(|r| r.dwForwardDest == 0 && r.dwForwardMask == 0 && r.dwForwardNextHop != 0)
            .min_by_key(|r| r.dwForwardMetric1)
            // the address is stored in network byte order, which is the in-memory byte order
            .map(|r| std::net::Ipv4Addr::from(r.dwForwardNextHop.to_ne_bytes()))
    }
}

/// `Wed Nov 18 11:06:40 2026 (local time)`, for the automatic CTCP TIME reply.
pub fn local_ctime() -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let t = unsafe { GetLocalTime() };
    format!(
        "{} {} {:2} {:02}:{:02}:{:02} {} (local time)",
        DAYS[(t.wDayOfWeek % 7) as usize],
        MONTHS[(t.wMonth.clamp(1, 12) - 1) as usize],
        t.wDay,
        t.wHour,
        t.wMinute,
        t.wSecond,
        t.wYear
    )
}

/// A server timestamp (`2024-05-05T10:00:09.000Z`, UTC) as local `MM-DD HH:MM`, for history lines.
pub fn history_stamp(iso: &str) -> Option<String> {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::System::Time::SystemTimeToTzSpecificLocalTime;
    let (y, mo, d, h, mi, s) = crate::history::parse_iso(iso)?;
    let utc = SYSTEMTIME { wYear: y, wMonth: mo, wDayOfWeek: 0, wDay: d, wHour: h, wMinute: mi, wSecond: s, wMilliseconds: 0 };
    let mut local = SYSTEMTIME::default();
    unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()? };
    Some(format!("{:02}-{:02} {:02}:{:02}", local.wMonth, local.wDay, local.wHour, local.wMinute))
}

/// The time a live line was sent according to the server (`server-time`), as local `HH:MM`; with the date
/// in front when it is not from today (a bouncer replaying what was said while we were away).
pub fn server_time_stamp(iso: &str) -> Option<String> {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::System::Time::SystemTimeToTzSpecificLocalTime;
    let (y, mo, d, h, mi, s) = crate::history::parse_iso(iso)?;
    let utc = SYSTEMTIME { wYear: y, wMonth: mo, wDayOfWeek: 0, wDay: d, wHour: h, wMinute: mi, wSecond: s, wMilliseconds: 0 };
    let mut local = SYSTEMTIME::default();
    unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()? };
    let now = unsafe { GetLocalTime() };
    if (local.wYear, local.wMonth, local.wDay) == (now.wYear, now.wMonth, now.wDay) {
        Some(format!("{:02}:{:02}", local.wHour, local.wMinute))
    } else {
        Some(format!("{:02}-{:02} {:02}:{:02}", local.wMonth, local.wDay, local.wHour, local.wMinute))
    }
}

/// Seconds since the program started (the clock starts the first time this is asked, which main does at start-up).
pub fn uptime_secs() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs()
}

/// Scale a 96-dpi pixel value to the system DPI.
pub fn px(v: i32) -> i32 {
    let dpi = unsafe { GetDpiForSystem() } as i32;
    v * dpi.max(96) / 96
}

/// Local `YYYY-MM-DD HH:MM:SS`, for log files.
pub fn datetime_stamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// "1.5 MB", "812 KB", "37 bytes" (binary units, one decimal from MB up).
pub fn human_size(n: u64) -> String {
    const KB: f64 = 1024.0;
    let f = n as f64;
    if n < 1024 {
        format!("{n} bytes")
    } else if f < KB * KB {
        format!("{:.0} KB", f / KB)
    } else if f < KB * KB * KB {
        format!("{:.1} MB", f / (KB * KB))
    } else {
        format!("{:.2} GB", f / (KB * KB * KB))
    }
}

pub fn timestamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!("{:02}:{:02}", t.wHour, t.wMinute)
}

/// Parse a DCC port range: `5000-5100`, or a single port `5000`. Ports below 1024 are rejected.
pub fn parse_port_range(s: &str) -> Option<(u16, u16)> {
    let s = s.trim();
    let (lo, hi) = match s.split_once('-') {
        Some((a, b)) => (a.trim().parse::<u16>().ok()?, b.trim().parse::<u16>().ok()?),
        None => {
            let p = s.parse::<u16>().ok()?;
            (p, p)
        }
    };
    (lo >= 1024 && lo <= hi).then_some((lo, hi))
}

/// Split text into chunks of at most `max` bytes on char boundaries.
pub fn split_text(s: &str, max: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    while !rest.is_empty() {
        if rest.len() <= max {
            out.push(rest);
            break;
        }
        let mut cut = max;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push(&rest[..cut]);
        rest = &rest[cut..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_ranges() {
        assert_eq!(parse_port_range("5000-5100"), Some((5000, 5100)));
        assert_eq!(parse_port_range(" 5000 - 5100 "), Some((5000, 5100)));
        assert_eq!(parse_port_range("6000"), Some((6000, 6000)));
        for bad in ["", "abc", "100-200", "6000-5000", "5000-99999", "1023", "-5"] {
            assert_eq!(parse_port_range(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn sizes_are_readable() {
        assert_eq!(human_size(0), "0 bytes");
        assert_eq!(human_size(1023), "1023 bytes");
        assert_eq!(human_size(1024), "1 KB");
        assert_eq!(human_size(1536 * 1024), "1.5 MB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.00 GB");
    }

    /// Reads this machine's routing table (read-only, nothing is sent anywhere).
    #[test]
    fn the_default_gateway_is_a_usable_address_or_absent() {
        match default_gateway() {
            Some(ip) => {
                eprintln!("default gateway on this machine: {ip}");
                assert!(!ip.is_unspecified() && !ip.is_broadcast() && !ip.is_multicast(), "{ip}");
            }
            None => eprintln!("no default gateway on this machine"),
        }
    }

    #[test]
    fn history_times_are_shown_in_local_time() {
        let s = history_stamp("2024-05-05T10:00:00.000Z").expect("a valid time converts");
        assert_eq!(s.len(), 11, "{s}");
        assert_eq!((&s[2..3], &s[5..6], &s[8..9]), ("-", " ", ":"), "{s}");
        // the offset from UTC is at most +/-14 hours, so the hour moves by that much at most
        let hour: i32 = s[6..8].parse().unwrap();
        let diff = (hour - 10).rem_euclid(24);
        assert!(diff <= 14 || diff >= 10, "{s}"); // within a day's wrap of UTC
        assert_eq!(history_stamp("not a time"), None);
        assert_eq!(history_stamp("2024-05-05T10:00:00+02:00"), None);
    }

    #[test]
    fn splits_on_boundaries() {
        assert_eq!(split_text("ééé", 3), ["é", "é", "é"]);
        assert_eq!(split_text("abcdef", 4), ["abcd", "ef"]);
        assert!(split_text("", 4).is_empty());
    }
}
