//! Pasting several lines into a chat, and pasting a picture: the pure parts (splitting the text, wording for the
//! question asked first, turning the clipboard's picture into a bitmap file). Unit-tested.

/// The most lines one paste may send: more than this is a job for a paste site, and would flood the server.
pub const MAX_LINES: usize = 50;
/// How many files one drop may offer.
pub const MAX_DROPPED_FILES: usize = 5;

/// The lines of pasted text, in order: line endings of any kind are understood, empty lines (and spaces at the
/// end of a line) are dropped, because an empty message cannot be sent.
pub fn split_lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .split(['\n', '\r'])
        .map(|l| l.trim_end().to_owned())
        .filter(|l| !l.trim().is_empty())
        .collect()
}

/// Several lines are worth a question first; a single line pastes as it always did.
pub fn needs_question(lines: &[String]) -> bool {
    lines.len() > 1
}

/// The first words of a line, for the question (control characters out, cut with `...`).
fn glimpse(line: &str) -> String {
    let flat: String = line.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let mut s: String = flat.chars().take(50).collect();
    if flat.chars().count() > 50 {
        s.push_str("...");
    }
    s
}

/// The question put to the user before several lines go out as separate messages to `target`.
pub fn question(lines: &[String], target: &str) -> String {
    let more = lines.len().saturating_sub(1);
    format!(
        "You are about to send {} lines to {target}, one message each:\n\n  {}\n  ...and {more} more\n\n\
         Yes: send them (one about every second, so the server does not mind)\n\
         No: put them in the entry box as one line instead\n\
         Cancel: do nothing",
        lines.len(),
        glimpse(&lines[0]),
    )
}

/// The lines as a single line, for the entry box.
pub fn joined(lines: &[String]) -> String {
    lines.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ")
}

/// A bitmap file (`.bmp`) made of the clipboard's DIB (a `BITMAPINFOHEADER` and the pixels): the 14-byte file
/// header is put in front. `None` when the data is too short or odd.
pub fn bmp_from_dib(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |o: usize| dib.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let u16_at = |o: usize| dib.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let header = u32_at(0)? as usize;
    if header < 40 || header > 124 || dib.len() < header {
        return None;
    }
    let bits = u16_at(14)?;
    let compression = u32_at(16)?;
    let used = u32_at(32)? as usize;
    if !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32) {
        return None;
    }
    let colours = if bits <= 8 { if used == 0 { 1usize << bits } else { used } } else { used };
    let masks = if compression == 3 && header == 40 { 12 } else { 0 }; // BI_BITFIELDS with the short header
    let offset = 14 + header + masks + colours * 4;
    if offset - 14 > dib.len() || dib.len() > 512 << 20 {
        return None;
    }
    let mut out = Vec::with_capacity(14 + dib.len());
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((14 + dib.len()) as u32).to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(offset as u32).to_le_bytes());
    out.extend_from_slice(dib);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_split_whatever_the_line_endings_and_blanks_are_dropped() {
        assert_eq!(split_lines("one\r\ntwo\nthree\rfour"), ["one", "two", "three", "four"]);
        assert_eq!(split_lines("a\n\n  \nb  \n"), ["a", "b"]);
        assert_eq!(split_lines("\n\n"), Vec::<String>::new());
        assert_eq!(split_lines("  indented\nx"), ["  indented", "x"], "leading spaces stay");
        assert_eq!(split_lines("single"), ["single"]);
        assert_eq!(split_lines(""), Vec::<String>::new());
    }

    #[test]
    fn only_several_lines_need_a_question() {
        assert!(!needs_question(&split_lines("just one")));
        assert!(!needs_question(&split_lines("one\n")));
        assert!(needs_question(&split_lines("one\ntwo")));
    }

    #[test]
    fn the_question_names_the_target_the_count_and_a_glimpse_without_control_characters() {
        let lines = split_lines(&format!("{}\nsecond\nthird", "x".repeat(80)));
        let q = question(&lines, "#rust");
        assert!(q.contains("3 lines to #rust") && q.contains("...and 2 more"), "{q}");
        assert!(q.contains(&format!("{}...", "x".repeat(50))) && !q.contains(&"x".repeat(51)), "{q}");
        let q = question(&["a\u{7}b".to_owned(), "c".to_owned()], "bob");
        assert!(!q.contains('\u{7}'), "{q:?}");
    }

    #[test]
    fn lines_are_joined_with_single_spaces() {
        assert_eq!(joined(&["a".into(), "  b ".into(), "c".into()]), "a b c");
    }

    fn dib(bits: u16, compression: u32, used: u32, pixel_bytes: usize) -> Vec<u8> {
        let mut d = vec![0u8; 40];
        d[0..4].copy_from_slice(&40u32.to_le_bytes());
        d[4..8].copy_from_slice(&2i32.to_le_bytes());
        d[8..12].copy_from_slice(&2i32.to_le_bytes());
        d[12..14].copy_from_slice(&1u16.to_le_bytes());
        d[14..16].copy_from_slice(&bits.to_le_bytes());
        d[16..20].copy_from_slice(&compression.to_le_bytes());
        d[32..36].copy_from_slice(&used.to_le_bytes());
        let extra = match (bits, compression) {
            (b, _) if b <= 8 => (if used == 0 { 1usize << b } else { used as usize }) * 4,
            (_, 3) => 12,
            _ => used as usize * 4,
        };
        d.extend(std::iter::repeat(7u8).take(extra + pixel_bytes));
        d
    }

    #[test]
    fn a_dib_becomes_a_bitmap_file_with_the_pixels_where_the_header_says() {
        let d = dib(24, 0, 0, 16);
        let f = bmp_from_dib(&d).unwrap();
        assert_eq!(&f[0..2], b"BM");
        assert_eq!(u32::from_le_bytes(f[2..6].try_into().unwrap()) as usize, f.len());
        assert_eq!(u32::from_le_bytes(f[10..14].try_into().unwrap()), 14 + 40, "pixels follow the header");
        assert_eq!(&f[14..], &d[..]);
        // 8 bits: a 256 entry colour table sits in between
        let f = bmp_from_dib(&dib(8, 0, 0, 4)).unwrap();
        assert_eq!(u32::from_le_bytes(f[10..14].try_into().unwrap()), 14 + 40 + 1024);
        // 32 bits with bit masks (BI_BITFIELDS): three masks after the header
        let f = bmp_from_dib(&dib(32, 3, 0, 8)).unwrap();
        assert_eq!(u32::from_le_bytes(f[10..14].try_into().unwrap()), 14 + 40 + 12);
    }

    #[test]
    fn damaged_or_odd_clipboard_pictures_are_refused() {
        assert!(bmp_from_dib(&[]).is_none());
        assert!(bmp_from_dib(&[1, 2, 3]).is_none());
        let mut bad = dib(24, 0, 0, 4);
        bad[0..4].copy_from_slice(&4u32.to_le_bytes()); // header size too small
        assert!(bmp_from_dib(&bad).is_none());
        assert!(bmp_from_dib(&dib(7, 0, 0, 4)).is_none(), "7 bits per pixel does not exist");
        // a colour table that runs past the data
        let mut short = dib(8, 0, 0, 0);
        short.truncate(60);
        assert!(bmp_from_dib(&short).is_none());
    }
}
