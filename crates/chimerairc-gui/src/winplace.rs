//! Where the main window was when it was last closed: position, size and whether it was maximized.
//! Pure data and geometry, unit-tested; `ui.rs` reads the real window and the monitors.
//!
//! The values are Win32 "restored" window coordinates in device pixels. Before they are used they
//! are checked against the monitors that exist now, so a window saved on a screen that has since
//! been unplugged (or at a resolution that has since shrunk) never ends up out of reach.

pub const MIN_W: i32 = 480;
pub const MIN_H: i32 = 320;
const LIMIT: i32 = 100_000;
/// The top strip of the window that must stay on a screen (title bar and menu), and the least of
/// it that has to be visible, in device pixels.
const STRIP_H: i32 = 40;
const SEEN_W: i32 = 100;
const SEEN_H: i32 = 20;

/// A screen's usable area: left, top, right, bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rc {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Rc {
    pub fn w(&self) -> i32 {
        self.r - self.l
    }
    pub fn h(&self) -> i32 {
        self.b - self.t
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

impl Placement {
    /// A `settings.ini` value: TAB-separated `left top width height maximized`.
    pub fn to_setting(&self) -> String {
        format!("{}\t{}\t{}\t{}\t{}", self.left, self.top, self.width, self.height, u8::from(self.maximized))
    }

    /// Parse a stored value; anything out of range (or a size too small to be a real window) is refused.
    pub fn from_setting(v: &str) -> Option<Placement> {
        let mut p = v.split('\t');
        let mut num = || p.next()?.trim().parse::<i32>().ok().filter(|n| n.abs() <= LIMIT);
        let (left, top, width, height) = (num()?, num()?, num()?, num()?);
        let maximized = match p.next()?.trim() {
            "1" => true,
            "0" => false,
            _ => return None,
        };
        (width >= MIN_W && height >= MIN_H).then_some(Placement { left, top, width, height, maximized })
    }
}

/// Overlap of two rectangles as (width, height); zero when they do not meet.
fn overlap(a: Rc, b: Rc) -> (i32, i32) {
    ((a.r.min(b.r) - a.l.max(b.l)).max(0), (a.b.min(b.b) - a.t.max(b.t)).max(0))
}

/// Make `p` safe to use on the given screens (the first is the primary one): the title strip has to
/// be reachable on some screen, else the window goes to the middle of the primary one, and it is
/// never smaller than the minimum or larger than the screen it ends up on.
pub fn fit(p: Placement, screens: &[Rc]) -> Placement {
    fit_min(p, screens, MIN_W, MIN_H)
}

/// [`fit`] for a window with its own smallest size.
pub fn fit_min(p: Placement, screens: &[Rc], min_w: i32, min_h: i32) -> Placement {
    let mut p = Placement { width: p.width.max(min_w), height: p.height.max(min_h), ..p };
    let Some(primary) = screens.first().copied() else { return p };
    let strip = Rc { l: p.left, t: p.top, r: p.left + p.width, b: p.top + STRIP_H };
    let reachable = screens.iter().any(|s| {
        let (w, h) = overlap(strip, *s);
        w >= SEEN_W.min(p.width) && h >= SEEN_H
    });
    if reachable {
        return p;
    }
    p.width = p.width.min(primary.w().max(min_w));
    p.height = p.height.min(primary.h().max(min_h));
    p.left = primary.l + (primary.w() - p.width) / 2;
    p.top = primary.t + (primary.h() - p.height) / 2;
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Rc = Rc { l: 0, t: 0, r: 1920, b: 1040 };
    const SECOND: Rc = Rc { l: 1920, t: 0, r: 3840, b: 1040 };

    fn win(left: i32, top: i32, width: i32, height: i32) -> Placement {
        Placement { left, top, width, height, maximized: false }
    }

    #[test]
    fn the_settings_format_roundtrips_including_negative_positions() {
        for p in [win(100, 80, 1024, 700), Placement { maximized: true, ..win(-1500, -20, 800, 600) }] {
            assert_eq!(Placement::from_setting(&p.to_setting()), Some(p));
        }
    }

    #[test]
    fn junk_and_tiny_windows_are_refused() {
        for junk in ["", "1\t2\t3", "a\t0\t800\t600\t0", "0\t0\t800\t600", "0\t0\t100\t600\t0", "0\t0\t800\t50\t0", "0\t0\t800\t600\t", "999999\t0\t800\t600\t0", "0\t0\t99999999999\t600\t0"] {
            assert_eq!(Placement::from_setting(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn a_window_that_is_reachable_is_left_alone() {
        let p = win(200, 100, 1000, 700);
        assert_eq!(fit(p, &[MAIN]), p);
        // on the second screen when it exists, also with a negative left edge on a left-hand screen
        assert_eq!(fit(win(2000, 50, 1000, 700), &[MAIN, SECOND]), win(2000, 50, 1000, 700));
        let left = Rc { l: -1920, t: 0, r: 0, b: 1040 };
        assert_eq!(fit(win(-1800, 10, 900, 600), &[MAIN, left]), win(-1800, 10, 900, 600));
        // partly off the edge is fine while the title strip can still be grabbed
        let edge = win(1800, 100, 1000, 700);
        assert_eq!(fit(edge, &[MAIN]), edge, "120 px of the title strip can still be grabbed");
        assert_ne!(fit(win(1850, 100, 1000, 700), &[MAIN]).left, 1850, "70 px is too little");
    }

    #[test]
    fn a_window_from_a_missing_screen_is_brought_back_to_the_middle_of_the_primary() {
        let gone = win(2500, 100, 1000, 700); // was on the second screen, which is unplugged
        let f = fit(gone, &[MAIN]);
        assert_eq!((f.left, f.top, f.width, f.height), (460, 170, 1000, 700));
        // scrolled up out of sight
        let up = fit(win(100, -2000, 1000, 700), &[MAIN]);
        assert_eq!(up.top, 170);
        // only a sliver of the title strip on screen
        assert_eq!(fit(win(1900, 100, 1000, 700), &[MAIN]).left, 460);
    }

    #[test]
    fn size_is_limited_to_the_minimum_and_to_the_screen_it_lands_on() {
        let big = fit(win(5000, 0, 6000, 4000), &[MAIN]);
        assert_eq!((big.width, big.height), (1920, 1040));
        assert_eq!((big.left, big.top), (0, 0));
        let small = fit(win(0, 0, 100, 100), &[MAIN]);
        assert_eq!((small.width, small.height), (MIN_W, MIN_H));
        // no screen information at all: only the minimum size is enforced
        assert_eq!(fit(win(7, 8, 900, 600), &[]), win(7, 8, 900, 600));
    }

    #[test]
    fn the_maximized_flag_survives_fitting() {
        let p = Placement { maximized: true, ..win(9000, 0, 800, 600) };
        assert!(fit(p, &[MAIN]).maximized);
    }
}
