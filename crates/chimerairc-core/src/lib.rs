//! ChimeraIRC client engine.
//!
//! * [`Session`]: sans-IO protocol state machine (registration, CAP, SASL, channel state).
//! * [`net`]: tokio + rustls transport that drives a `Session` and yields [`Event`]s.

mod config;
pub mod clientcert;
pub mod dcc;
pub mod dcctls;
mod event;
mod ignore;
mod isupport;
pub mod natpmp;
pub mod net;
pub mod portmap;
pub mod presence;
pub mod reconnect;
pub mod scram;
pub mod sts;
mod session;
mod state;
pub mod upnp;

pub use config::Config;
pub use event::{ChatKind, Event, HistoryLine, ModeChange, TypingState};
pub use ignore::{IgnoreKind, IgnoreRule};
pub use isupport::{CaseMapping, ISupport};
pub use session::Session;
pub use state::{Channel, Member};
