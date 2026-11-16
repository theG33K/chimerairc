/// Connection settings for one server.
#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    /// TLS is the default (port 6697). Plaintext must be opted into.
    pub tls: bool,
    pub nick: String,
    /// Nicknames to try, in order, when the server refuses `nick` (or the one before) during registration;
    /// when they run out, `_` is added to the last one tried.
    pub alt_nicks: Vec<String>,
    pub user: String,
    pub realname: String,
    /// Server password (PASS), if any.
    pub password: Option<String>,
    /// SASL credentials: (account, password). SCRAM-SHA-256 is used when the server offers it (the
    /// password then never crosses the connection), otherwise PLAIN, which sends the password itself:
    /// over `tls: false` that means in cleartext.
    pub sasl: Option<(String, String)>,
    /// CTCP VERSION reply text.
    pub version_reply: String,
    /// Ignore rules in force from the start (update later with `Client::set_ignores`).
    pub ignores: Vec<crate::IgnoreRule>,
    /// The text of an automatic CTCP TIME reply. The default is the current time in UTC; a front end
    /// with access to the local time zone replaces it. (A function, so the session stays free of clocks.)
    pub time_text: fn() -> String,
    /// A monotonic clock (time since some fixed start), used to rate-limit automatic CTCP replies.
    pub monotonic: fn() -> std::time::Duration,
    /// At most this many automatic CTCP replies per window; more are dropped silently (a CTCP flood
    /// must not make us flood the network or get us kicked). `(0, _)` turns every automatic reply off.
    pub ctcp_reply_limit: (u32, std::time::Duration),
    /// Source of the client nonce for SCRAM (a function so that tests can fix it).
    pub scram_nonce: fn() -> String,
    /// How many lines of history to ask for when joining a channel on a server with `chathistory`;
    /// 0 asks for none.
    pub history_on_join: u32,
    /// A TLS client certificate, presented during the handshake and used for SASL EXTERNAL. Only used
    /// when `tls` is on.
    pub client_cert: Option<crate::clientcert::ClientCert>,
    /// Extra trusted root certificates (a server with its own certificate authority, or a self-signed
    /// certificate you have decided to trust), in addition to the built-in public roots.
    pub extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>,
}

/// `Thu Jan  1 00:00:00 1970 UTC` for a count of seconds since the Unix epoch.
pub fn utc_ctime(secs: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]; // 1970-01-01 was a Thursday
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // civil-from-days (Howard Hinnant's algorithm), days counted from 1970-01-01
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{} {} {:2} {:02}:{:02}:{:02} {} UTC",
        DAYS[(days % 7) as usize],
        MONTHS[(month - 1) as usize],
        day,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        year
    )
}

fn default_time_text() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    utc_ctime(secs)
}

fn default_monotonic() -> std::time::Duration {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed()
}

impl Config {
    pub fn new(host: &str, nick: &str) -> Config {
        Config {
            host: host.to_owned(),
            port: 6697,
            tls: true,
            nick: nick.to_owned(),
            user: nick.to_owned(),
            realname: nick.to_owned(),
            password: None,
            sasl: None,
            version_reply: concat!("ChimeraIRC ", env!("CARGO_PKG_VERSION")).to_owned(),
            ignores: Vec::new(),
            time_text: default_time_text,
            monotonic: default_monotonic,
            ctcp_reply_limit: (5, std::time::Duration::from_secs(10)),
            scram_nonce: crate::scram::random_nonce,
            history_on_join: 50,
            alt_nicks: Vec::new(),
            client_cert: None,
            extra_roots: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::utc_ctime;

    #[test]
    fn utc_ctime_matches_known_dates() {
        assert_eq!(utc_ctime(0), "Thu Jan  1 00:00:00 1970 UTC");
        assert_eq!(utc_ctime(951_782_400), "Tue Feb 29 00:00:00 2000 UTC", "2000 was a leap year");
        assert_eq!(utc_ctime(1_700_000_000), "Tue Nov 14 22:13:20 2023 UTC");
        assert_eq!(utc_ctime(1_795_000_000), "Wed Nov 18 11:06:40 2026 UTC");
        assert_eq!(utc_ctime(4_102_444_799), "Thu Dec 31 23:59:59 2099 UTC");
    }
}
