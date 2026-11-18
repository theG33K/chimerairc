//! Looks: light and dark themes, the colours of the chat text, and the fonts. Pure data and arithmetic,
//! unit-tested; `look.rs` puts it on the screen.

use chimerairc_proto::format::Rgb;

/// Which theme the user picked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Choice {
    #[default]
    Light,
    Dark,
    /// Follow the Windows setting for apps.
    System,
}

impl Choice {
    pub fn name(self) -> &'static str {
        match self {
            Choice::Light => "light",
            Choice::Dark => "dark",
            Choice::System => "system",
        }
    }
    pub fn parse(s: &str) -> Option<Choice> {
        match s.trim().to_ascii_lowercase().as_str() {
            "light" => Some(Choice::Light),
            "dark" => Some(Choice::Dark),
            "system" => Some(Choice::System),
            _ => None,
        }
    }
}

/// The colours that make up a look.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    pub dark: bool,
    /// Background of the chat text, the lists and the entry boxes.
    pub bg: Rgb,
    /// Ordinary text on `bg`.
    pub fg: Rgb,
    /// Background of windows, dock panels, tabs, the toolbar and the status bar.
    pub panel: Rgb,
    pub panel_text: Rgb,
    /// A selected tab, and lines in the lists that are selected.
    pub accent: Rgb,
    pub accent_text: Rgb,
    // chat line colours
    pub info: Rgb,
    pub action: Rgb,
    pub notice: Rgb,
    pub error: Rgb,
    pub timestamp: Rgb,
    /// Lines around tabs, boxes and panels.
    pub border: Rgb,
    // nick list colours by channel rank
    pub owner: Rgb,
    pub admin: Rgb,
    pub op: Rgb,
    pub halfop: Rgb,
    pub voice: Rgb,
}

/// Chat colours the user chose themselves; `None` keeps the theme's.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Overrides {
    pub bg: Option<Rgb>,
    pub fg: Option<Rgb>,
    pub info: Option<Rgb>,
    pub action: Option<Rgb>,
    pub notice: Option<Rgb>,
    pub error: Option<Rgb>,
    pub timestamp: Option<Rgb>,
    pub owner: Option<Rgb>,
    pub admin: Option<Rgb>,
    pub op: Option<Rgb>,
    pub halfop: Option<Rgb>,
    pub voice: Option<Rgb>,
}

/// The names of the colours that can be overridden, as stored and as shown.
pub const COLOR_NAMES: [&str; 12] = ["bg", "fg", "info", "action", "notice", "error", "timestamp", "owner", "admin", "op", "halfop", "voice"];

impl Overrides {
    pub fn get(&self, name: &str) -> Option<Rgb> {
        match name {
            "bg" => self.bg,
            "fg" => self.fg,
            "info" => self.info,
            "action" => self.action,
            "notice" => self.notice,
            "error" => self.error,
            "timestamp" => self.timestamp,
            "owner" => self.owner,
            "admin" => self.admin,
            "op" => self.op,
            "halfop" => self.halfop,
            "voice" => self.voice,
            _ => None,
        }
    }
    pub fn set(&mut self, name: &str, c: Option<Rgb>) -> bool {
        let slot = match name {
            "bg" => &mut self.bg,
            "fg" => &mut self.fg,
            "info" => &mut self.info,
            "action" => &mut self.action,
            "notice" => &mut self.notice,
            "error" => &mut self.error,
            "timestamp" => &mut self.timestamp,
            "owner" => &mut self.owner,
            "admin" => &mut self.admin,
            "op" => &mut self.op,
            "halfop" => &mut self.halfop,
            "voice" => &mut self.voice,
            _ => return false,
        };
        *slot = c;
        true
    }
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        *self == Overrides::default()
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb(r, g, b)
}

impl Theme {
    /// The look the program always had.
    pub const LIGHT: Theme = Theme {
        dark: false,
        bg: rgb(0xff, 0xff, 0xff),
        fg: rgb(0x00, 0x00, 0x00),
        panel: rgb(0xf0, 0xf0, 0xf0),
        panel_text: rgb(0x00, 0x00, 0x00),
        accent: rgb(0xff, 0xff, 0xff),
        accent_text: rgb(0x00, 0x00, 0x00),
        info: rgb(0x60, 0x60, 0x60),
        action: rgb(0x9c, 0x00, 0x9c),
        notice: rgb(0xb0, 0x50, 0x00),
        error: rgb(0xc0, 0x00, 0x00),
        timestamp: rgb(0x80, 0x80, 0x80),
        border: rgb(0xb4, 0xb4, 0xb4),
        owner: rgb(0x7b, 0x1f, 0xa2),
        admin: rgb(0xc6, 0x28, 0x28),
        op: rgb(0x1b, 0x7f, 0x3b),
        halfop: rgb(0xa8, 0x62, 0x00),
        voice: rgb(0x15, 0x65, 0xc0),
    };

    pub const DARK: Theme = Theme {
        dark: true,
        bg: rgb(0x1e, 0x1e, 0x1e),
        fg: rgb(0xd8, 0xd8, 0xd8),
        panel: rgb(0x2b, 0x2b, 0x2b),
        panel_text: rgb(0xe2, 0xe2, 0xe2),
        accent: rgb(0x1e, 0x1e, 0x1e),
        accent_text: rgb(0xff, 0xff, 0xff),
        info: rgb(0x9a, 0x9a, 0x9a),
        action: rgb(0xd6, 0x8a, 0xd6),
        notice: rgb(0xe0, 0xa0, 0x5a),
        error: rgb(0xff, 0x6b, 0x6b),
        timestamp: rgb(0x7c, 0x7c, 0x7c),
        border: rgb(0x48, 0x48, 0x48),
        owner: rgb(0xc7, 0x92, 0xea),
        admin: rgb(0xff, 0x6b, 0x6b),
        op: rgb(0x6f, 0xd0, 0x8c),
        halfop: rgb(0xe0, 0xa0, 0x5a),
        voice: rgb(0x6c, 0xb2, 0xff),
    };

    /// The theme for a choice, with the user's own chat colours on top. `system_dark` says whether Windows is
    /// set to dark apps.
    pub fn resolve(choice: Choice, system_dark: bool, o: &Overrides) -> Theme {
        let dark = match choice {
            Choice::Light => false,
            Choice::Dark => true,
            Choice::System => system_dark,
        };
        let mut t = if dark { Theme::DARK } else { Theme::LIGHT };
        t.bg = o.bg.unwrap_or(t.bg);
        t.fg = o.fg.unwrap_or(t.fg);
        t.info = o.info.unwrap_or(t.info);
        t.action = o.action.unwrap_or(t.action);
        t.notice = o.notice.unwrap_or(t.notice);
        t.error = o.error.unwrap_or(t.error);
        t.timestamp = o.timestamp.unwrap_or(t.timestamp);
        t.owner = o.owner.unwrap_or(t.owner);
        t.admin = o.admin.unwrap_or(t.admin);
        t.op = o.op.unwrap_or(t.op);
        t.halfop = o.halfop.unwrap_or(t.halfop);
        t.voice = o.voice.unwrap_or(t.voice);
        // a chosen background and no chosen text colour must still be readable
        if o.bg.is_some() && o.fg.is_none() {
            t.fg = readable_on(t.fg, t.bg);
        }
        // the rank colours must stay readable on whatever background is in use
        for c in [&mut t.owner, &mut t.admin, &mut t.op, &mut t.halfop, &mut t.voice] {
            *c = readable_on(*c, t.bg);
        }
        t
    }

    /// The colour of a nick in the user list for its rank symbol (`~` owner, `&` admin, `@` op, `%` half-op,
    /// `+` voice); `None` for an ordinary member, who gets the normal text colour.
    pub fn rank_color(&self, sym: &str) -> Option<Rgb> {
        match sym.chars().next()? {
            '~' => Some(self.owner),
            '&' => Some(self.admin),
            '@' => Some(self.op),
            '%' => Some(self.halfop),
            '+' => Some(self.voice),
            _ => None,
        }
    }

    /// A colour that comes with the text (mIRC colours, highlight colours) made readable on this theme's
    /// background.
    pub fn readable(&self, c: Rgb) -> Rgb {
        readable_on(c, self.bg)
    }
}

// ---------------------------------------------------------------- colours

/// `#rrggbb`, `rrggbb` or `#rgb`; `None` for anything else.
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    match h.len() {
        6 if h.is_ascii() => Some(Rgb(byte(0)?, byte(2)?, byte(4)?)),
        3 if h.is_ascii() => {
            let n = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| v * 17);
            Some(Rgb(n(0)?, n(1)?, n(2)?))
        }
        _ => None,
    }
}

pub fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

/// Relative luminance (WCAG): 0 for black, 1 for white.
pub fn luminance(c: Rgb) -> f64 {
    let lin = |v: u8| {
        let s = v as f64 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.0) + 0.7152 * lin(c.1) + 0.0722 * lin(c.2)
}

/// Contrast ratio of two colours: 1 (the same) to 21 (black on white).
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

pub fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let m = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round().clamp(0.0, 255.0) as u8;
    Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// Text colour `fg` on background `bg`, moved toward white (on a dark background) or black (on a light one)
/// just far enough to be easy to read. Colours that are readable already are returned unchanged.
pub fn readable_on(fg: Rgb, bg: Rgb) -> Rgb {
    const WANTED: f64 = 4.5;
    if contrast(fg, bg) >= WANTED {
        return fg;
    }
    let toward = if luminance(bg) < 0.4 { Rgb(255, 255, 255) } else { Rgb(0, 0, 0) };
    let mut t = 0.0;
    let mut c = fg;
    while t < 1.0 && contrast(c, bg) < WANTED {
        t += 0.05;
        c = mix(fg, toward, t);
    }
    c
}

// ---------------------------------------------------------------- fonts

/// A font as chosen by the user: face name, size in points and whether it is bold.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FontSpec {
    pub face: String,
    pub points: u32,
    pub bold: bool,
}

pub const MIN_POINTS: u32 = 6;
pub const MAX_POINTS: u32 = 32;

impl FontSpec {
    pub fn new(face: &str, points: u32, bold: bool) -> FontSpec {
        FontSpec { face: clean_face(face), points: points.clamp(MIN_POINTS, MAX_POINTS), bold }
    }

    /// The default for chat text, and for the interface.
    pub fn chat_default() -> FontSpec {
        FontSpec::new("Consolas", 10, false)
    }
    pub fn ui_default() -> FontSpec {
        FontSpec::new("Segoe UI", 9, false)
    }

    /// One `settings.ini` value: `face|points|bold`.
    pub fn to_setting(&self) -> String {
        format!("{}|{}|{}", self.face, self.points, u8::from(self.bold))
    }

    pub fn from_setting(v: &str) -> Option<FontSpec> {
        let mut p = v.split('|');
        let face = clean_face(p.next()?);
        let points: u32 = p.next()?.trim().parse().ok()?;
        let bold = match p.next()?.trim() {
            "1" => true,
            "0" => false,
            _ => return None,
        };
        (!face.is_empty() && (MIN_POINTS..=MAX_POINTS).contains(&points)).then_some(FontSpec { face, points, bold })
    }
}

/// A font face name: no control characters or `|`, at most 31 characters (Windows' limit).
fn clean_face(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() && *c != '|').take(31).collect::<String>().trim().to_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn ranks_have_their_own_readable_colours() {
        for t in [Theme::LIGHT, Theme::DARK] {
            let all: Vec<_> = ["~", "&", "@", "%", "+"].iter().map(|s| t.rank_color(s).unwrap()).collect();
            for (i, a) in all.iter().enumerate() {
                assert!(contrast(*a, t.bg) >= 4.5, "rank {i} unreadable");
                for b in &all[i + 1..] {
                    assert_ne!(a, b);
                }
            }
            assert_eq!(t.rank_color(""), None);
            assert_eq!(t.rank_color("x"), None);
        }
    }

    #[test]
    fn rank_colours_can_be_chosen_and_stay_readable() {
        let mut o = Overrides::default();
        assert!(o.set("op", Some(Rgb(1, 2, 3))));
        let t = Theme::resolve(Choice::Light, false, &o);
        assert_eq!(t.rank_color("@"), Some(Rgb(1, 2, 3)));
        // a dark colour on a chosen dark background is lightened
        o.bg = Some(Rgb(0, 0, 0));
        let t = Theme::resolve(Choice::Light, false, &o);
        assert!(contrast(t.rank_color("@").unwrap(), Rgb(0, 0, 0)) >= 4.5);
    }

    use super::*;

    #[test]
    fn hex_colours_parse_in_the_usual_forms_and_nothing_else() {
        assert_eq!(parse_hex("#1e2f3a"), Some(Rgb(0x1e, 0x2f, 0x3a)));
        assert_eq!(parse_hex("FFAA00"), Some(Rgb(255, 170, 0)));
        assert_eq!(parse_hex(" #fa0 "), Some(Rgb(255, 170, 0)));
        for bad in ["", "#", "#12345", "#1234567", "#gg0000", "red", "#12 456", "ééé", "#ééééé"] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
        assert_eq!(hex(Rgb(1, 2, 255)), "#0102ff");
        assert_eq!(parse_hex(&hex(Rgb(9, 99, 199))), Some(Rgb(9, 99, 199)));
    }

    #[test]
    fn contrast_is_the_usual_ratio() {
        assert!((contrast(Rgb(0, 0, 0), Rgb(255, 255, 255)) - 21.0).abs() < 0.01);
        assert!((contrast(Rgb(10, 20, 30), Rgb(10, 20, 30)) - 1.0).abs() < 1e-9);
        // both built-in themes read well
        for t in [Theme::LIGHT, Theme::DARK] {
            assert!(contrast(t.fg, t.bg) >= 7.0, "{t:?}");
            assert!(contrast(t.panel_text, t.panel) >= 7.0, "{t:?}");
            for c in [t.info, t.action, t.notice, t.error] {
                assert!(contrast(c, t.bg) >= 4.0, "{c:?} on {:?}", t.bg);
            }
        }
    }

    #[test]
    fn unreadable_colours_are_moved_just_far_enough_and_good_ones_are_left_alone() {
        let dark = Theme::DARK.bg;
        let black_on_dark = readable_on(Rgb(0, 0, 0), dark);
        assert!(contrast(black_on_dark, dark) >= 4.5);
        assert!(black_on_dark.0 > 0x40, "black text is lightened on a dark background: {black_on_dark:?}");
        let navy = readable_on(Rgb(0, 0, 0x7f), dark);
        assert!(contrast(navy, dark) >= 4.5);
        let yellow_on_white = readable_on(Rgb(0xff, 0xff, 0x00), Rgb(255, 255, 255));
        assert!(contrast(yellow_on_white, Rgb(255, 255, 255)) >= 4.5);
        assert!(yellow_on_white.0 < 0xe0, "yellow is darkened on a light background");
        // already fine: untouched
        assert_eq!(readable_on(Rgb(0xd8, 0xd8, 0xd8), dark), Rgb(0xd8, 0xd8, 0xd8));
        assert_eq!(readable_on(Rgb(0, 0, 0), Rgb(255, 255, 255)), Rgb(0, 0, 0));
    }

    #[test]
    fn a_theme_follows_the_choice_and_the_windows_setting() {
        let none = Overrides::default();
        assert!(!Theme::resolve(Choice::Light, true, &none).dark);
        assert!(Theme::resolve(Choice::Dark, false, &none).dark);
        assert!(Theme::resolve(Choice::System, true, &none).dark);
        assert!(!Theme::resolve(Choice::System, false, &none).dark);
        assert_eq!(Theme::resolve(Choice::Light, false, &none), Theme::LIGHT);
    }

    #[test]
    fn the_users_own_colours_override_the_theme_and_stay_readable() {
        let mut o = Overrides::default();
        assert!(o.is_empty());
        assert!(o.set("bg", parse_hex("#000000")));
        assert!(o.set("info", parse_hex("#00ff00")));
        assert!(!o.set("nonsense", None));
        let t = Theme::resolve(Choice::Light, false, &o);
        assert_eq!(t.bg, Rgb(0, 0, 0));
        assert_eq!(t.info, Rgb(0, 255, 0));
        assert_eq!(t.error, Theme::LIGHT.error, "others keep the theme's");
        assert!(contrast(t.fg, t.bg) >= 4.5, "text is moved off black when only the background was chosen");
        // a text colour that was chosen is respected even if it reads badly
        o.set("fg", parse_hex("#101010"));
        assert_eq!(Theme::resolve(Choice::Light, false, &o).fg, Rgb(0x10, 0x10, 0x10));
        assert_eq!(o.get("info"), Some(Rgb(0, 255, 0)));
        for n in COLOR_NAMES {
            let mut x = Overrides::default();
            assert!(x.set(n, Some(Rgb(1, 2, 3))));
            assert_eq!(x.get(n), Some(Rgb(1, 2, 3)));
        }
    }

    #[test]
    fn colours_that_come_with_text_are_made_readable_on_the_background() {
        let t = Theme::DARK;
        let c = t.readable(Rgb(0, 0, 0));
        assert!(contrast(c, t.bg) >= 4.5);
        assert_eq!(Theme::LIGHT.readable(Rgb(0, 0, 0)), Rgb(0, 0, 0));
    }

    #[test]
    fn choices_and_fonts_roundtrip_through_the_settings() {
        for c in [Choice::Light, Choice::Dark, Choice::System] {
            assert_eq!(Choice::parse(c.name()), Some(c));
        }
        assert_eq!(Choice::parse(" DARK "), Some(Choice::Dark));
        assert_eq!(Choice::parse("blue"), None);
        let f = FontSpec::new("Cascadia Mono", 11, true);
        assert_eq!(FontSpec::from_setting(&f.to_setting()), Some(f));
        for junk in ["", "Consolas", "Consolas|x|0", "Consolas|10", "Consolas|10|2", "|10|0", "Consolas|3|0", "Consolas|99|0"] {
            assert_eq!(FontSpec::from_setting(junk), None, "{junk:?}");
        }
        // names are cleaned and sizes limited
        let f = FontSpec::new("Bad|Face\u{7}Name", 500, false);
        assert_eq!(f.face, "BadFaceName");
        assert_eq!(f.points, MAX_POINTS);
        assert_eq!(FontSpec::new("x", 1, false).points, MIN_POINTS);
        assert_eq!(FontSpec::new(&"n".repeat(80), 10, false).face.len(), 31);
    }
}
