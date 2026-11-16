//! Channel and member tracking.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub nick: String,
    /// Prefix mode letters held, in server rank order (highest first), e.g. "ov".
    pub modes: String,
}

#[derive(Debug, Clone)]
pub struct Channel {
    pub name: String,
    pub topic: Option<String>,
    /// Keyed by case-folded nick.
    pub members: HashMap<String, Member>,
    /// Mode letters currently set (types B/C/D only; lists are not tracked).
    pub modes: String,
    /// True after 366 (end of NAMES) so a UI can show the list as complete.
    pub names_complete: bool,
}

impl Channel {
    pub fn new(name: &str) -> Channel {
        Channel {
            name: name.to_owned(),
            topic: None,
            members: HashMap::new(),
            modes: String::new(),
            names_complete: false,
        }
    }
}
