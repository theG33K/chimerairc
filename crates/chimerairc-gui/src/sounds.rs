//! Notification sounds: which sound plays for which event, and playing it.
//!
//! A sound is written as text, in the settings and in the dialogs:
//! * `off` (or `none`): silent;
//! * a name from `NAMES`: one of the sounds Windows already has (`asterisk`, `message`, `mail`, ...);
//! * the path of a `.wav` file.
//!
//! The text handling is pure and tested; `play` hands the result to Windows.

/// An event that can make a sound.
pub struct Event {
    pub id: &'static str,
    pub label: &'static str,
    /// What plays when the user has not chosen anything.
    pub default: &'static str,
}

pub const EVENTS: [Event; 7] = [
    Event { id: "highlight", label: "Highlight", default: "off" },
    Event { id: "private", label: "Private message", default: "off" },
    Event { id: "invite", label: "Invitation", default: "off" },
    Event { id: "dcc_request", label: "DCC offer", default: "off" },
    Event { id: "dcc_done", label: "Transfer finished", default: "off" },
    Event { id: "connected", label: "Connected", default: "off" },
    Event { id: "disconnected", label: "Disconnected", default: "off" },
];

/// The sounds Windows has, by the short name used here, with the system's alias for each.
pub const NAMES: [(&str, &str); 10] = [
    ("asterisk", "SystemAsterisk"),
    ("exclamation", "SystemExclamation"),
    ("hand", "SystemHand"),
    ("question", "SystemQuestion"),
    ("default", "SystemDefault"),
    ("notify", "Notification.Default"),
    ("message", "Notification.IM"),
    ("mail", "Notification.Mail"),
    ("reminder", "Notification.Reminder"),
    ("sms", "Notification.SMS"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Spec {
    Off,
    /// A system sound, by its alias.
    Alias(&'static str),
    /// A `.wav` file.
    File(String),
}

pub fn event(id: &str) -> Option<&'static Event> {
    EVENTS.iter().find(|e| e.id == id)
}

fn looks_like_path(s: &str) -> bool {
    s.contains(['\\', '/', ':']) || s.to_ascii_lowercase().ends_with(".wav")
}

/// Read a sound written as text. `Err` says what is wrong, in words for the user.
pub fn parse(text: &str) -> Result<Spec, String> {
    let t = text.trim().trim_matches('"');
    if t.is_empty() || t.eq_ignore_ascii_case("off") || t.eq_ignore_ascii_case("none") {
        return Ok(Spec::Off);
    }
    if let Some((_, alias)) = NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(t)) {
        return Ok(Spec::Alias(alias));
    }
    if looks_like_path(t) {
        if !t.to_ascii_lowercase().ends_with(".wav") {
            return Err(format!("\"{t}\" is not a .wav file; only .wav sounds can be played."));
        }
        return Ok(Spec::File(t.to_owned()));
    }
    let names: Vec<&str> = NAMES.iter().map(|(n, _)| *n).collect();
    Err(format!("\"{t}\" is not a sound. Use off, one of {}, or the path of a .wav file.", names.join(", ")))
}

/// Like `parse`, and for a file also checks that it is there.
pub fn check(text: &str) -> Result<Spec, String> {
    let spec = parse(text)?;
    if let Spec::File(p) = &spec {
        if !std::path::Path::new(p).is_file() {
            return Err(format!("The file \"{p}\" was not found."));
        }
    }
    Ok(spec)
}

/// The sound for an event: the user's choice if there is one, else the event's default.
pub fn choice_for<'a>(chosen: &'a [(String, String)], id: &str) -> &'a str {
    chosen.iter().find(|(e, _)| e == id).map(|(_, s)| s.as_str()).or_else(|| event(id).map(|e| e.default)).unwrap_or("off")
}

/// Play a sound (does not wait for it to end). Anything that cannot be played is ignored.
pub fn play(text: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
    let Ok(spec) = parse(text) else { return };
    unsafe {
        match spec {
            Spec::Off => {}
            Spec::Alias(a) => {
                let w = crate::util::wide(a);
                let _ = PlaySoundW(PCWSTR(w.as_ptr()), None, SND_ALIAS | SND_ASYNC | SND_NODEFAULT);
            }
            Spec::File(p) => {
                let w = crate::util::wide(&p);
                let _ = PlaySoundW(PCWSTR(w.as_ptr()), None, SND_FILENAME | SND_ASYNC | SND_NODEFAULT);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_off_and_files_are_told_apart() {
        assert_eq!(parse("off"), Ok(Spec::Off));
        assert_eq!(parse("  NONE "), Ok(Spec::Off));
        assert_eq!(parse(""), Ok(Spec::Off));
        assert_eq!(parse("Message"), Ok(Spec::Alias("Notification.IM")));
        assert_eq!(parse("asterisk"), Ok(Spec::Alias("SystemAsterisk")));
        assert_eq!(parse(r#""C:\Sounds\ping.wav""#), Ok(Spec::File(r"C:\Sounds\ping.wav".into())));
        assert_eq!(parse("ping.WAV"), Ok(Spec::File("ping.WAV".into())));
    }

    #[test]
    fn bad_sounds_say_what_is_wrong() {
        assert!(parse("boing").unwrap_err().contains("is not a sound"));
        assert!(parse(r"C:\x\song.mp3").unwrap_err().contains(".wav"));
        assert!(check(r"C:\definitely\missing.wav").unwrap_err().contains("not found"));
        assert_eq!(check("hand"), Ok(Spec::Alias("SystemHand")));
    }

    #[test]
    fn every_event_has_a_playable_default() {
        for e in &EVENTS {
            assert!(parse(e.default).is_ok(), "{}", e.id);
        }
        for (n, a) in NAMES {
            assert_eq!(parse(n), Ok(Spec::Alias(a)));
        }
        let mut ids: Vec<_> = EVENTS.iter().map(|e| e.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), EVENTS.len());
    }

    #[test]
    fn the_users_choice_beats_the_default() {
        let chosen = vec![("private".to_owned(), "mail".to_owned())];
        assert_eq!(choice_for(&chosen, "private"), "mail");
        assert_eq!(choice_for(&chosen, "highlight"), "off");
        assert_eq!(choice_for(&chosen, "nonsense"), "off");
    }
}
