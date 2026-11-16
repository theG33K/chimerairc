//! Highlight rules: find words in incoming chat and colour them (or the whole line).
//!
//! Pure logic, no Win32. Offsets are byte offsets into the *plain* (formatting-stripped) text.

use chimerairc_proto::format::{Rgb, Span};

/// One user-defined highlight (modelled on HydraIRC's: substring / whole line / case sensitive).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub word: String,
    pub case_sensitive: bool,
    /// Match only whole words (so "bob" does not match "bobby").
    pub whole_word: bool,
    /// Colour the whole line instead of just the matched text.
    pub whole_line: bool,
    pub color: Rgb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub start: usize,
    pub end: usize,
    pub color: Rgb,
}

/// Result of scanning one message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    pub hits: Vec<Hit>,
    /// Set when a whole-line rule matched.
    pub line: Option<Rgb>,
}

impl Marks {
    pub fn is_empty(&self) -> bool {
        self.hits.is_empty() && self.line.is_none()
    }

    /// Move all offsets right by `by` bytes (the message text follows a prefix like `<nick> `).
    pub fn shifted(mut self, by: usize) -> Marks {
        for h in &mut self.hits {
            h.start += by;
            h.end += by;
        }
        self
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || "_-[]\\`^{|}".contains(c)
}

/// Byte ranges where `word` occurs in `text`. Case-insensitive matching is ASCII-only (offsets
/// stay valid); non-ASCII letters compare exactly.
fn find_all(text: &str, word: &str, case_sensitive: bool, whole_word: bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let n = word.len();
    if n == 0 || n > text.len() {
        return out;
    }
    let (tb, wb) = (text.as_bytes(), word.as_bytes());
    let mut i = 0;
    while i + n <= tb.len() {
        let hit = text.is_char_boundary(i)
            && text.is_char_boundary(i + n)
            && if case_sensitive { &tb[i..i + n] == wb } else { tb[i..i + n].eq_ignore_ascii_case(wb) };
        if hit {
            let before_ok = !whole_word || text[..i].chars().next_back().is_none_or(|c| !is_word_char(c));
            let after_ok = !whole_word || text[i + n..].chars().next().is_none_or(|c| !is_word_char(c));
            if before_ok && after_ok {
                out.push((i, i + n));
                i += n;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Scan plain message text against `rules` (later rules win where they overlap).
pub fn scan(plain: &str, rules: &[Rule]) -> Marks {
    let mut m = Marks::default();
    for r in rules {
        let found = find_all(plain, &r.word, r.case_sensitive, r.whole_word);
        if found.is_empty() {
            continue;
        }
        if r.whole_line {
            m.line = Some(r.color);
        } else {
            m.hits.extend(found.into_iter().map(|(start, end)| Hit { start, end, color: r.color }));
        }
    }
    m
}

/// Split `spans` so every highlighted run carries its colour (and bold). The spans' text must
/// concatenate to the plain text the marks were computed on.
pub fn apply(spans: Vec<Span>, marks: &Marks) -> Vec<Span> {
    if marks.hits.is_empty() {
        return spans;
    }
    let total: usize = spans.iter().map(|s| s.text.len()).sum();
    let mut paint: Vec<Option<Rgb>> = vec![None; total];
    for h in &marks.hits {
        for p in paint.iter_mut().take(h.end.min(total)).skip(h.start.min(total)) {
            *p = Some(h.color);
        }
    }
    let mut out = Vec::new();
    let mut base = 0;
    for span in spans {
        let mut run = String::new();
        let mut run_color: Option<Rgb> = None;
        let mut first = true;
        let flush = |out: &mut Vec<Span>, run: &mut String, color: Option<Rgb>| {
            if run.is_empty() {
                return;
            }
            let mut style = span.style.clone();
            if let Some(c) = color {
                style.fg = Some(c);
                style.bold = true;
            }
            out.push(Span { text: std::mem::take(run), style });
        };
        for (off, ch) in span.text.char_indices() {
            let color = paint.get(base + off).copied().flatten();
            if !first && color != run_color {
                flush(&mut out, &mut run, run_color);
            }
            run_color = color;
            first = false;
            run.push(ch);
        }
        flush(&mut out, &mut run, run_color);
        base += span.text.len();
    }
    out
}

// ---------------------------------------------------------------- colours

const NAMED: &[(&str, Rgb)] = &[
    ("red", Rgb(0xc0, 0x00, 0x00)),
    ("orange", Rgb(0xd0, 0x60, 0x00)),
    ("gold", Rgb(0xb0, 0x80, 0x00)),
    ("green", Rgb(0x00, 0x80, 0x00)),
    ("teal", Rgb(0x00, 0x70, 0x70)),
    ("blue", Rgb(0x00, 0x00, 0xc0)),
    ("purple", Rgb(0x80, 0x00, 0xa0)),
    ("pink", Rgb(0xc0, 0x00, 0x80)),
    ("brown", Rgb(0x80, 0x40, 0x00)),
    ("grey", Rgb(0x60, 0x60, 0x60)),
    ("black", Rgb(0x00, 0x00, 0x00)),
];

/// Accepts a colour name (`red`, `teal`, ...) or hex (`RRGGBB`, `#RRGGBB`).
pub fn parse_color(s: &str) -> Option<Rgb> {
    let t = s.trim();
    if let Some((_, c)) = NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(t)) {
        return Some(*c);
    }
    let hex = t.strip_prefix('#').unwrap_or(t);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// Name for a colour if it is one of the presets, else `RRGGBB`.
pub fn color_name(c: Rgb) -> String {
    NAMED
        .iter()
        .find(|(_, v)| *v == c)
        .map_or_else(|| format!("{:02X}{:02X}{:02X}", c.0, c.1, c.2), |(n, _)| (*n).to_owned())
}

impl Rule {
    /// `FLAGS|RRGGBB|word` where FLAGS is a subset of `c` (case), `w` (whole word), `l` (line), or `-`.
    pub fn to_setting(&self) -> String {
        let mut f = String::new();
        if self.case_sensitive {
            f.push('c');
        }
        if self.whole_word {
            f.push('w');
        }
        if self.whole_line {
            f.push('l');
        }
        if f.is_empty() {
            f.push('-');
        }
        let word: String = self.word.chars().filter(|c| !c.is_control()).collect();
        format!("{f}|{:02X}{:02X}{:02X}|{word}", self.color.0, self.color.1, self.color.2)
    }

    pub fn from_setting(s: &str) -> Option<Rule> {
        let mut it = s.splitn(3, '|');
        let (flags, color, word) = (it.next()?, it.next()?, it.next()?);
        if word.trim().is_empty() {
            return None;
        }
        Some(Rule {
            word: word.to_owned(),
            case_sensitive: flags.contains('c'),
            whole_word: flags.contains('w'),
            whole_line: flags.contains('l'),
            color: parse_color(color)?,
        })
    }

    /// One-line description for the rule list.
    pub fn describe(&self) -> String {
        let mut opts = vec![color_name(self.color)];
        if self.whole_word {
            opts.push("whole word".into());
        }
        if self.case_sensitive {
            opts.push("case sensitive".into());
        }
        if self.whole_line {
            opts.push("whole line".into());
        }
        format!("{}   ({})", self.word, opts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chimerairc_proto::format::{parse, strip};

    const RED: Rgb = Rgb(0xc0, 0, 0);
    const BLUE: Rgb = Rgb(0, 0, 0xc0);

    fn rule(word: &str) -> Rule {
        Rule { word: word.into(), case_sensitive: false, whole_word: false, whole_line: false, color: RED }
    }

    #[test]
    fn substring_and_whole_word() {
        let t = "bobby and Bob and bob.";
        assert_eq!(scan(t, &[rule("bob")]).hits.len(), 3);
        let mut r = rule("bob");
        r.whole_word = true;
        let hits = scan(t, &[r]).hits;
        assert_eq!(hits.iter().map(|h| (h.start, h.end)).collect::<Vec<_>>(), [(10, 13), (18, 21)]);
    }

    #[test]
    fn case_sensitivity() {
        let mut r = rule("Bob");
        r.case_sensitive = true;
        assert_eq!(scan("bob Bob", &[r.clone()]).hits.len(), 1);
        r.case_sensitive = false;
        assert_eq!(scan("bob Bob", &[r]).hits.len(), 2);
    }

    #[test]
    fn whole_line_rule_sets_line_colour_only() {
        let mut r = rule("urgent");
        r.whole_line = true;
        r.color = BLUE;
        let m = scan("this is URGENT", &[r]);
        assert_eq!(m.line, Some(BLUE));
        assert!(m.hits.is_empty());
        assert!(scan("calm", &[rule("urgent")]).is_empty());
    }

    #[test]
    fn non_ascii_text_keeps_valid_offsets() {
        let t = "héllo wörld bob";
        let h = scan(t, &[rule("bob")]).hits;
        assert_eq!(&t[h[0].start..h[0].end], "bob");
        assert_eq!(scan("naïve", &[rule("ï")]).hits.len(), 1);
        assert!(scan("x", &[rule("")]).is_empty());
    }

    #[test]
    fn apply_splits_spans_and_keeps_existing_style() {
        let text = "hi \x02bold bob\x02 bye";
        let plain = strip(text);
        let marks = scan(&plain, &[rule("bob")]);
        let spans = apply(parse(text), &marks);
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, plain);
        let bob = spans.iter().find(|s| s.text == "bob").expect("bob is its own span");
        assert_eq!(bob.style.fg, Some(RED));
        assert!(bob.style.bold);
        let plain_run = spans.iter().find(|s| s.text == "hi ").unwrap();
        assert_eq!(plain_run.style.fg, None);
        assert!(!plain_run.style.bold);
    }

    #[test]
    fn shifted_marks_line_up_after_a_prefix() {
        let prefix = "<alice> ";
        let body = "ping bob";
        let marks = scan(body, &[rule("bob")]).shifted(prefix.len());
        let line = format!("{prefix}{body}");
        let spans = apply(parse(&line), &marks);
        assert_eq!(spans.iter().find(|s| s.style.fg == Some(RED)).unwrap().text, "bob");
    }

    #[test]
    fn later_rules_win_on_overlap() {
        let mut blue = rule("bob");
        blue.color = BLUE;
        let spans = apply(parse("bob"), &scan("bob", &[rule("bob"), blue]));
        assert_eq!(spans[0].style.fg, Some(BLUE));
    }

    #[test]
    fn colours_parse_and_name() {
        assert_eq!(parse_color("Red"), Some(RED));
        assert_eq!(parse_color("#00ff80"), Some(Rgb(0, 255, 128)));
        assert_eq!(parse_color("00FF80"), Some(Rgb(0, 255, 128)));
        assert_eq!(parse_color("nope"), None);
        assert_eq!(parse_color("12345"), None);
        assert_eq!(color_name(RED), "red");
        assert_eq!(color_name(Rgb(1, 2, 3)), "010203");
    }

    #[test]
    fn settings_roundtrip() {
        let r = Rule { word: "a|b c".into(), case_sensitive: true, whole_word: true, whole_line: false, color: BLUE };
        assert_eq!(Rule::from_setting(&r.to_setting()), Some(r));
        let plain = rule("x");
        assert!(plain.to_setting().starts_with("-|"));
        assert_eq!(Rule::from_setting(&plain.to_setting()), Some(plain));
        assert_eq!(Rule::from_setting("-|zzzzzz|word"), None);
        assert_eq!(Rule::from_setting("-|C00000|  "), None);
        assert_eq!(Rule::from_setting("garbage"), None);
    }
}
