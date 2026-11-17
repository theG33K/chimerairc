//! Looking for a newer release on GitHub: one HTTPS request for the list of releases, a small JSON reader, and a
//! comparison of version numbers. Nothing is downloaded or installed; the caller only learns that a newer version
//! exists and where its page is.
//!
//! The request carries only a generic `User-Agent` (the program name and version); it sends no cookies, no account
//! and nothing about the machine. It is made only when the user asks (or has switched the start-up check on).

use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Most bytes read from the server: a release list is a few kilobytes.
pub const MAX_RESPONSE: usize = 1 << 20;
/// Longest the whole check may take.
pub const TIMEOUT: Duration = Duration::from_secs(15);

// ------------------------------------------------------------------------------------------------- JSON

/// A JSON value (just enough of the format for GitHub's answers).
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(v) => v.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(v) => Some(v),
            _ => None,
        }
    }
}

const MAX_DEPTH: usize = 24;

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn ws(&mut self) {
        while self.b.get(self.i).is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", c as char, self.i))
        }
    }
    fn word(&mut self, w: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(format!("unexpected text at byte {}", self.i))
        }
    }
    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err("nested too deeply".into());
        }
        self.ws();
        match self.b.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Obj(items));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    self.eat(b':')?;
                    items.push((k, self.value(depth + 1)?));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Obj(items));
                        }
                        _ => return Err(format!("expected ',' or '}}' at byte {}", self.i)),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Arr(items));
                        }
                        _ => return Err(format!("expected ',' or ']' at byte {}", self.i)),
                    }
                }
            }
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.word("true", Json::Bool(true)),
            Some(b'f') => self.word("false", Json::Bool(false)),
            Some(b'n') => self.word("null", Json::Null),
            Some(c) if *c == b'-' || c.is_ascii_digit() => {
                let start = self.i;
                while self.b.get(self.i).is_some_and(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E')) {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[start..self.i]).ok().and_then(|s| s.parse::<f64>().ok()).map(Json::Num).ok_or_else(|| format!("bad number at byte {start}"))
            }
            _ => Err(format!("unexpected input at byte {}", self.i)),
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.b.get(self.i..self.i + 4).and_then(|s| std::str::from_utf8(s).ok()).and_then(|s| u32::from_str_radix(s, 16).ok());
        self.i += 4;
        h.ok_or_else(|| "bad \\u escape".to_string())
    }
    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&c) = self.b.get(self.i) else { return Err("unterminated string".into()) };
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|_| "the text is not valid UTF-8".to_string()),
                b'\\' => {
                    let Some(&e) = self.b.get(self.i) else { return Err("unterminated escape".into()) };
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) {
                                // a surrogate pair: the low half follows as another \u escape
                                if self.b.get(self.i..self.i + 2) != Some(b"\\u") {
                                    return Err("lone surrogate".into());
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err("bad surrogate pair".into());
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            char::from_u32(cp).ok_or_else(|| "bad code point".to_string())?
                        }
                        _ => return Err("bad escape".into()),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return Err("control character in a string".into()),
                c => out.push(c),
            }
        }
    }
}

/// Parse a JSON text. Strict, limited in depth, and it must be the whole text.
pub fn parse_json(text: &str) -> Result<Json, String> {
    let mut r = Reader { b: text.as_bytes(), i: 0 };
    let v = r.value(0)?;
    r.ws();
    if r.i != r.b.len() {
        return Err("text after the JSON value".into());
    }
    Ok(v)
}

// ------------------------------------------------------------------------------------------------- versions

/// The numbers of a version tag: `v0.2.1` and `0.2.1-beta.3` both give `[0, 2, 1]`.
pub fn parse_version(tag: &str) -> Option<Vec<u64>> {
    let t = tag.trim().trim_start_matches(['v', 'V']);
    let core = t.split(['-', '+', ' ']).next()?;
    let nums: Option<Vec<u64>> = core.split('.').map(|p| p.parse::<u64>().ok()).collect();
    nums.filter(|n| !n.is_empty() && n.len() <= 4)
}

/// Compare two version number lists, a missing part counting as 0 (`1.0` equals `1.0.0`).
pub fn cmp_versions(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x.cmp(&y);
        }
    }
    std::cmp::Ordering::Equal
}

/// Is the release tagged `tag` newer than `current`?
pub fn is_newer(current: &str, tag: &str) -> bool {
    match (parse_version(current), parse_version(tag)) {
        (Some(c), Some(t)) => cmp_versions(&t, &c) == std::cmp::Ordering::Greater,
        _ => false,
    }
}

// ------------------------------------------------------------------------------------------------- releases

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub name: String,
    pub url: String,
    pub prerelease: bool,
}

/// The newest published release in a GitHub `releases` answer (drafts are skipped), by version number; `None` for an
/// empty list or one with no usable tag.
pub fn newest_release(json: &str) -> Result<Option<Release>, String> {
    let v = parse_json(json)?;
    let list = v.as_arr().ok_or("the answer is not a list of releases")?;
    let mut best: Option<(Vec<u64>, Release)> = None;
    for r in list {
        if r.get("draft").and_then(Json::as_bool) == Some(true) {
            continue;
        }
        let Some(tag) = r.get("tag_name").and_then(Json::as_str) else { continue };
        let Some(ver) = parse_version(tag) else { continue };
        let release = Release {
            tag: tag.to_owned(),
            name: r.get("name").and_then(Json::as_str).unwrap_or(tag).to_owned(),
            url: r.get("html_url").and_then(Json::as_str).unwrap_or_default().to_owned(),
            prerelease: r.get("prerelease").and_then(Json::as_bool).unwrap_or(false),
        };
        if best.as_ref().map_or(true, |(b, _)| cmp_versions(&ver, b) == std::cmp::Ordering::Greater) {
            best = Some((ver, release));
        }
    }
    Ok(best.map(|(_, r)| r))
}

// ------------------------------------------------------------------------------------------------- HTTP

/// Undo `Transfer-Encoding: chunked`.
fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let eol = body.windows(2).position(|w| w == b"\r\n").ok_or("a broken chunked answer")?;
        let size_text = std::str::from_utf8(&body[..eol]).map_err(|_| "a broken chunk size")?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16).map_err(|_| "a broken chunk size")?;
        body = &body[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size + 2 || out.len() + size > MAX_RESPONSE {
            return Err("a broken or oversized chunked answer".into());
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

/// Turn a raw HTTP/1.1 answer into its body: only `200 OK` is accepted.
pub fn http_body(raw: &[u8]) -> Result<String, String> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("the answer has no end of headers")?;
    let head = std::str::from_utf8(&raw[..split]).map_err(|_| "the headers are not text")?;
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or("");
    let code: u16 = status.split(' ').nth(1).and_then(|c| c.parse().ok()).ok_or("an unreadable status line")?;
    match code {
        200 => {}
        403 | 429 => return Err("GitHub says too many requests were made from this address; try again later".into()),
        404 => return Err("the release list was not found".into()),
        c => return Err(format!("the server answered with status {c}")),
    }
    let chunked = lines.any(|l| l.to_ascii_lowercase().replace(' ', "").starts_with("transfer-encoding:chunked"));
    let body = &raw[split + 4..];
    let bytes = if chunked { dechunk(body)? } else { body.to_vec() };
    String::from_utf8(bytes).map_err(|_| "the answer is not valid text".into())
}

/// Send the request over an open (TLS) stream and return the body of the answer.
pub async fn fetch<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, host: &str, path: &str, user_agent: &str) -> Result<String, String> {
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {user_agent}\r\nAccept: application/vnd.github+json\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.map_err(|e| format!("could not send the request: {e}"))?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = stream.read(&mut buf).await.map_err(|e| format!("could not read the answer: {e}"))?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if raw.len() > MAX_RESPONSE {
            return Err("the answer is too big".into());
        }
    }
    http_body(&raw)
}

/// Ask `host` (over TLS, verified against the public roots) for `path` and return the body of the answer.
pub async fn get(host: &str, path: &str, user_agent: &str) -> Result<String, String> {
    let run = async {
        let cfg = crate::net::tls_config(&crate::Config::new(host, "update")).map_err(|e| format!("TLS setup failed: {e}"))?;
        let tcp = tokio::net::TcpStream::connect((host, 443)).await.map_err(|e| format!("could not connect to {host}: {e}"))?;
        let name = rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(|e| e.to_string())?;
        let tls = tokio_rustls::TlsConnector::from(std::sync::Arc::new(cfg)).connect(name, tcp).await.map_err(|e| format!("the secure connection failed: {e}"))?;
        fetch(tls, host, path, user_agent).await
    };
    tokio::time::timeout(TIMEOUT, run).await.map_err(|_| "the check took too long".to_string())?
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing newer. `latest` is the newest release seen, when there is one.
    UpToDate { latest: Option<Release> },
    Newer(Release),
}

/// Decide from the releases JSON.
pub fn outcome(json: &str, current: &str) -> Result<Outcome, String> {
    Ok(match newest_release(json)? {
        Some(r) if is_newer(current, &r.tag) => Outcome::Newer(r),
        other => Outcome::UpToDate { latest: other },
    })
}

/// Check GitHub's release list of `repo` (`owner/name`) against `current`.
pub async fn check_github(repo: &str, current: &str) -> Result<Outcome, String> {
    let path = format!("/repos/{repo}/releases?per_page=20");
    let body = get("api.github.com", &path, &format!("ChimeraIRC/{current}")).await?;
    outcome(&body, current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn json_is_read_strictly() {
        let v = parse_json(r#" {"a": [1, -2.5e1, true, false, null, "x\ny\u00e9\ud83d\ude00"], "b": {"c": ""}} "#).unwrap();
        assert_eq!(v.get("a").unwrap().as_arr().unwrap().len(), 6);
        assert_eq!(v.get("a").unwrap().as_arr().unwrap()[5].as_str(), Some("x\ny\u{e9}\u{1F600}"));
        assert_eq!(v.get("b").and_then(|b| b.get("c")).and_then(Json::as_str), Some(""));
        assert_eq!(parse_json("[]"), Ok(Json::Arr(vec![])));
        assert_eq!(parse_json("{}"), Ok(Json::Obj(vec![])));
        for bad in ["", "{", "[1,]", "{\"a\":}", "[1] x", "\"unterminated", "\"bad\\q\"", "\"\\ud800\"", "\"a\u{1}b\"", "tru", "{'a':1}", "[1 2]"] {
            assert!(parse_json(bad).is_err(), "{bad:?}");
        }
        // deep nesting is refused rather than overflowing the stack
        assert!(parse_json(&"[".repeat(1000)).is_err());
    }

    #[test]
    fn versions_are_read_and_compared() {
        assert_eq!(parse_version("v0.2.1"), Some(vec![0, 2, 1]));
        assert_eq!(parse_version("0.2.1-beta.3"), Some(vec![0, 2, 1]));
        assert_eq!(parse_version(" V1.0 "), Some(vec![1, 0]));
        for bad in ["", "latest", "v", "1.x", "1.2.3.4.5", "..", "v-1"] {
            assert_eq!(parse_version(bad), None, "{bad:?}");
        }
        assert_eq!(cmp_versions(&[1, 0], &[1, 0, 0]), Equal);
        assert_eq!(cmp_versions(&[0, 10, 0], &[0, 9, 9]), Greater);
        assert_eq!(cmp_versions(&[0, 1, 0, 5], &[0, 1, 0, 6]), Less);
        assert!(is_newer("0.1.0", "v0.2.0") && is_newer("0.1.0", "0.1.1") && is_newer("0.9.0", "v0.10.0"));
        assert!(!is_newer("0.1.0", "v0.1.0") && !is_newer("0.2.0", "v0.1.9") && !is_newer("0.1.0", "nightly") && !is_newer("junk", "v1.0.0"));
    }

    const RELEASES: &str = r#"[
      {"tag_name":"v0.1.0","name":"ChimeraIRC 0.1.0","html_url":"https://github.com/o/r/releases/tag/v0.1.0","prerelease":true,"draft":false},
      {"tag_name":"v0.3.0","name":"Secret draft","html_url":"https://github.com/o/r/releases/tag/v0.3.0","prerelease":false,"draft":true},
      {"tag_name":"v0.2.0","name":"ChimeraIRC 0.2.0","html_url":"https://github.com/o/r/releases/tag/v0.2.0","prerelease":false,"draft":false},
      {"tag_name":"not-a-version","name":"x","html_url":"https://x","prerelease":false,"draft":false}
    ]"#;

    #[test]
    fn the_newest_published_release_wins_and_drafts_are_ignored() {
        let r = newest_release(RELEASES).unwrap().unwrap();
        assert_eq!((r.tag.as_str(), r.prerelease, r.name.as_str()), ("v0.2.0", false, "ChimeraIRC 0.2.0"));
        assert_eq!(r.url, "https://github.com/o/r/releases/tag/v0.2.0");
        assert_eq!(newest_release("[]"), Ok(None));
        assert!(newest_release("{}").is_err());
        assert!(newest_release("not json").is_err());
    }

    #[test]
    fn an_update_is_reported_only_when_one_is_newer() {
        assert!(matches!(outcome(RELEASES, "0.1.0"), Ok(Outcome::Newer(r)) if r.tag == "v0.2.0"));
        assert!(matches!(outcome(RELEASES, "0.2.0"), Ok(Outcome::UpToDate { latest: Some(r) }) if r.tag == "v0.2.0"));
        assert!(matches!(outcome(RELEASES, "1.0.0"), Ok(Outcome::UpToDate { .. })));
        assert_eq!(outcome("[]", "0.1.0"), Ok(Outcome::UpToDate { latest: None }));
    }

    #[test]
    fn http_answers_are_unwrapped_and_errors_are_explained() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n[]";
        assert_eq!(http_body(plain).unwrap(), "[]");
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n[1,\r\n2\r\n2]\r\n0\r\n\r\n";
        assert_eq!(http_body(chunked).unwrap(), "[1,2]");
        let case = b"HTTP/1.1 200 OK\r\ntransfer-encoding:  Chunked\r\n\r\n2\r\n[]\r\n0\r\n\r\n";
        assert_eq!(http_body(case).unwrap(), "[]");
        assert!(http_body(b"HTTP/1.1 403 Forbidden\r\n\r\n").unwrap_err().contains("too many requests"));
        assert!(http_body(b"HTTP/1.1 404 Not Found\r\n\r\n").unwrap_err().contains("not found"));
        assert!(http_body(b"HTTP/1.1 500 Oops\r\n\r\n").unwrap_err().contains("500"));
        assert!(http_body(b"garbage").is_err());
        assert!(http_body(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n").is_err());
        assert!(http_body(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10\r\nshort\r\n").is_err());
    }

    #[tokio::test]
    async fn the_request_is_generic_and_the_answer_is_read_to_the_end() {
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        let reply = tokio::spawn(async move {
            let mut got = Vec::new();
            let mut buf = [0u8; 1024];
            while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = server.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
            }
            server.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]").await.unwrap();
            server.shutdown().await.unwrap();
            String::from_utf8(got).unwrap()
        });
        let body = fetch(client, "api.example", "/repos/o/r/releases?per_page=20", "ChimeraIRC/0.1.0").await.unwrap();
        assert_eq!(body, "[]");
        let request = reply.await.unwrap();
        assert!(request.starts_with("GET /repos/o/r/releases?per_page=20 HTTP/1.1\r\n"), "{request}");
        assert!(request.contains("Host: api.example\r\n") && request.contains("User-Agent: ChimeraIRC/0.1.0\r\n"), "{request}");
        // nothing identifying beyond the program name: no cookies, no authorization
        let lower = request.to_ascii_lowercase();
        assert!(!lower.contains("cookie") && !lower.contains("authorization"), "{request}");
    }

    #[tokio::test]
    async fn an_oversized_answer_is_refused() {
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let _ = server.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await;
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..40 {
                if server.write_all(&chunk).await.is_err() {
                    break;
                }
            }
        });
        assert!(fetch(client, "h", "/p", "ua").await.unwrap_err().contains("too big"));
    }
}
