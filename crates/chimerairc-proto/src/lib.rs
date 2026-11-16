//! IRC wire protocol for ChimeraIRC.
//!
//! Targets the "Modern IRC Client Protocol" (<https://modern.ircdocs.horse>), which
//! supersedes RFC 1459 / RFC 2812 in practice, plus IRCv3 message tags.
//! This crate does no I/O and has no dependencies.

mod ctcp;
pub mod dcc;
pub mod format;
pub mod mask;
mod message;
mod prefix;

pub use ctcp::Ctcp;
pub use message::{EncodeError, Message, ParseError, Tag, MAX_LINE_LEN, MAX_TAGS_LEN};
pub use prefix::Source;
