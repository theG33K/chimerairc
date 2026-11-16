//! IRC text formatting: bold, italic, underline, strikethrough, monospace, reverse,
//! mIRC colours 0-98 (99 = default) and hex colours (`\x04RRGGBB[,RRGGBB]`).
//! See <https://modern.ircdocs.horse/formatting>.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub monospace: bool,
    pub reverse: bool,
    /// `None` means the default colour.
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

const fn rgb(v: u32) -> Rgb {
    Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// The 99-entry mIRC palette (0-15 classic, 16-98 extended).
const PALETTE: [Rgb; 99] = [
    rgb(0xffffff), rgb(0x000000), rgb(0x00007f), rgb(0x009300), rgb(0xff0000), rgb(0x7f0000),
    rgb(0x9c009c), rgb(0xfc7f00), rgb(0xffff00), rgb(0x00fc00), rgb(0x009393), rgb(0x00ffff),
    rgb(0x0000fc), rgb(0xff00ff), rgb(0x7f7f7f), rgb(0xd2d2d2),
    rgb(0x470000), rgb(0x472100), rgb(0x474700), rgb(0x324700), rgb(0x004700), rgb(0x00472c),
    rgb(0x004747), rgb(0x002747), rgb(0x000047), rgb(0x2e0047), rgb(0x470047), rgb(0x47002a),
    rgb(0x740000), rgb(0x743a00), rgb(0x747400), rgb(0x517400), rgb(0x007400), rgb(0x007449),
    rgb(0x007474), rgb(0x004074), rgb(0x000074), rgb(0x4b0074), rgb(0x740074), rgb(0x740045),
    rgb(0xb50000), rgb(0xb56300), rgb(0xb5b500), rgb(0x7db500), rgb(0x00b500), rgb(0x00b571),
    rgb(0x00b5b5), rgb(0x0063b5), rgb(0x0000b5), rgb(0x7500b5), rgb(0xb500b5), rgb(0xb5006b),
    rgb(0xff0000), rgb(0xff8c00), rgb(0xffff00), rgb(0xb2ff00), rgb(0x00ff00), rgb(0x00ffa0),
    rgb(0x00ffff), rgb(0x008cff), rgb(0x0000ff), rgb(0xa500ff), rgb(0xff00ff), rgb(0xff0098),
    rgb(0xff5959), rgb(0xffb459), rgb(0xffff71), rgb(0xcfff60), rgb(0x6fff6f), rgb(0x65ffc9),
    rgb(0x6dffff), rgb(0x59b4ff), rgb(0x5959ff), rgb(0xc459ff), rgb(0xff66ff), rgb(0xff59bc),
    rgb(0xff9c9c), rgb(0xffd39c), rgb(0xffff9c), rgb(0xe2ff9c), rgb(0x9cff9c), rgb(0x9cffdb),
    rgb(0x9cffff), rgb(0x9cd3ff), rgb(0x9c9cff), rgb(0xdc9cff), rgb(0xff9cff), rgb(0xff94d3),
    rgb(0x000000), rgb(0x131313), rgb(0x282828), rgb(0x363636), rgb(0x4d4d4d), rgb(0x656565),
    rgb(0x818181), rgb(0x9f9f9f), rgb(0xbcbcbc), rgb(0xe2e2e2), rgb(0xffffff),
];

/// Colour for an mIRC colour number; `None` for 99 (default) and anything out of range.
pub fn mirc_color(n: u8) -> Option<Rgb> {
    PALETTE.get(n as usize).copied()
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// Up to two decimal digits.
fn take_num(it: &mut Chars) -> Option<u8> {
    let mut n: Option<u8> = None;
    for _ in 0..2 {
        match it.peek().and_then(|c| c.to_digit(10)) {
            Some(d) => {
                n = Some(n.unwrap_or(0) * 10 + d as u8);
                it.next();
            }
            None => break,
        }
    }
    n
}

/// Exactly six hex digits, or nothing is consumed.
fn take_hex6(it: &mut Chars) -> Option<Rgb> {
    let mut look = it.clone();
    let mut v = 0u32;
    for _ in 0..6 {
        v = v * 16 + look.next()?.to_digit(16)?;
    }
    for _ in 0..6 {
        it.next();
    }
    Some(rgb(v))
}

/// True when the next two chars are `,` and a char satisfying `ok`.
fn comma_then(it: &Chars, ok: impl Fn(char) -> bool) -> bool {
    let mut look = it.clone();
    look.next() == Some(',') && look.next().is_some_and(ok)
}

/// Parse formatted text into styled runs. Unknown control characters are dropped.
pub fn parse(s: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut style = Style::default();
    let mut buf = String::new();
    let mut it = s.chars().peekable();

    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                spans.push(Span { text: std::mem::take(&mut buf), style: style.clone() });
            }
        };
    }

    while let Some(c) = it.next() {
        match c {
            '\x02' | '\x1d' | '\x1f' | '\x1e' | '\x11' | '\x16' | '\x0f' => {
                flush!();
                match c {
                    '\x02' => style.bold = !style.bold,
                    '\x1d' => style.italic = !style.italic,
                    '\x1f' => style.underline = !style.underline,
                    '\x1e' => style.strike = !style.strike,
                    '\x11' => style.monospace = !style.monospace,
                    '\x16' => style.reverse = !style.reverse,
                    _ => style = Style::default(),
                }
            }
            '\x03' => {
                flush!();
                match take_num(&mut it) {
                    None => {
                        style.fg = None;
                        style.bg = None;
                    }
                    Some(n) => {
                        style.fg = mirc_color(n);
                        if comma_then(&it, |c| c.is_ascii_digit()) {
                            it.next();
                            if let Some(b) = take_num(&mut it) {
                                style.bg = mirc_color(b);
                            }
                        }
                    }
                }
            }
            '\x04' => {
                flush!();
                match take_hex6(&mut it) {
                    None => {
                        style.fg = None;
                        style.bg = None;
                    }
                    Some(fg) => {
                        style.fg = Some(fg);
                        let mut look = it.clone();
                        if look.next() == Some(',') {
                            let mut after = look.clone();
                            if let Some(bg) = take_hex6(&mut after) {
                                it.next();
                                let _ = take_hex6(&mut it);
                                style.bg = Some(bg);
                            }
                        }
                    }
                }
            }
            '\t' => buf.push(' '),
            c if (c as u32) < 0x20 || c == '\x7f' => {}
            c => buf.push(c),
        }
    }
    flush!();
    spans
}

/// Text with all formatting removed.
pub fn strip(s: &str) -> String {
    parse(s).into_iter().map(|sp| sp.text).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(spans: &[Span]) -> Vec<(&str, bool, Option<Rgb>, Option<Rgb>)> {
        spans.iter().map(|s| (s.text.as_str(), s.style.bold, s.style.fg, s.style.bg)).collect()
    }

    #[test]
    fn plain_and_toggles() {
        let s = parse("a\x02b\x02c");
        assert_eq!(st(&s), [("a", false, None, None), ("b", true, None, None), ("c", false, None, None)]);
    }

    #[test]
    fn mirc_colours_fg_bg_and_reset() {
        let s = parse("\x0304,12red on blue\x03 plain \x034only fg\x0f!");
        assert_eq!(s[0].text, "red on blue");
        assert_eq!(s[0].style.fg, mirc_color(4));
        assert_eq!(s[0].style.bg, mirc_color(12));
        assert_eq!(s[1].style.fg, None); // bare \x03 resets
        assert_eq!(s[2].style.fg, mirc_color(4));
        assert_eq!(s[2].style.bg, None);
        assert_eq!(s[3].style, Style::default()); // \x0f
    }

    #[test]
    fn colour_edge_cases() {
        // Two digits max; a third is text.
        assert_eq!(parse("\x03123")[0].text, "3");
        // Comma not followed by a digit is text.
        let s = parse("\x033,x");
        assert_eq!(s[0].text, ",x");
        assert_eq!(s[0].style.fg, mirc_color(3));
        // 99 = default colour.
        assert_eq!(parse("\x0399hi")[0].style.fg, None);
        // Colour persists across other style toggles.
        let s = parse("\x034a\x02b");
        assert_eq!(s[1].style.fg, mirc_color(4));
        assert!(s[1].style.bold);
    }

    #[test]
    fn hex_colours() {
        let s = parse("\x04ff0000,00ff00x\x04y");
        assert_eq!(s[0].style.fg, Some(Rgb(255, 0, 0)));
        assert_eq!(s[0].style.bg, Some(Rgb(0, 255, 0)));
        assert_eq!(s[1].style.fg, None);
        // Incomplete hex is not consumed.
        assert_eq!(parse("\x04ff00zz")[0].text, "ff00zz");
    }

    #[test]
    fn strip_and_controls() {
        assert_eq!(strip("\x02bold\x02 \x0304,12red\x03 \x04ff0000hex\x0f\x07bell\tt"), "bold red hexbell t");
        assert_eq!(parse("").len(), 0);
    }

    #[test]
    fn palette_endpoints() {
        assert_eq!(mirc_color(0), Some(Rgb(255, 255, 255)));
        assert_eq!(mirc_color(98), Some(Rgb(255, 255, 255)));
        assert_eq!(mirc_color(52), Some(Rgb(255, 0, 0)));
        assert_eq!(mirc_color(99), None);
    }
}
