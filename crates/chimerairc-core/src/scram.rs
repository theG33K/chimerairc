//! SCRAM-SHA-256 (RFC 5802 with SHA-256 as in RFC 7677) for SASL: the client side only.
//!
//! The password never goes over the wire, not even for a moment: the client proves it knows it, and
//! the server proves it knows it too (the server signature is checked in constant time), so a fake
//! server that does not know the password cannot pass as the real one.
//!
//! Limits, on purpose: user names and passwords must be ASCII (full SASLprep is not implemented, so
//! other credentials use PLAIN instead), and the server's iteration count must be between 4096
//! (RFC 7677's minimum) and 1,000,000.

use base64::Engine;
use ring::{digest, hmac, pbkdf2};
use std::num::NonZeroU32;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
pub const MECHANISM: &str = "SCRAM-SHA-256";
pub const MIN_ITERATIONS: u32 = 4096;
pub const MAX_ITERATIONS: u32 = 1_000_000;

/// True when these credentials can be used with SCRAM here (ASCII only, see the module docs).
pub fn can_use(account: &str, password: &str) -> bool {
    account.is_ascii() && password.is_ascii() && !account.is_empty()
}

/// A fresh random nonce (24 base64 characters), for real connections.
pub fn random_nonce() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 18];
    // A failing system RNG is not something a client can recover from; a fixed nonce would be worse
    // than no login, so produce one from the clock instead of crashing (it is still unique per call).
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        bytes[..16].copy_from_slice(&n.to_be_bytes());
    }
    B64.encode(bytes)
}

/// `=` and `,` are special in SCRAM user names.
fn saslname(s: &str) -> String {
    s.replace('=', "=3D").replace(',', "=2C")
}

/// Equality that takes the same time wherever the first difference is.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub struct Scram {
    password: String,
    client_nonce: String,
    client_first_bare: String,
    expected_server_signature: Option<Vec<u8>>,
}

impl Scram {
    pub fn new(account: &str, password: &str, client_nonce: &str) -> Scram {
        Scram {
            password: password.to_owned(),
            client_nonce: client_nonce.to_owned(),
            client_first_bare: format!("n={},r={client_nonce}", saslname(account)),
            expected_server_signature: None,
        }
    }

    /// The first message: no channel binding, no authorisation identity.
    pub fn client_first(&self) -> String {
        format!("n,,{}", self.client_first_bare)
    }

    /// Answer the server's first message with the client proof.
    pub fn client_final(&mut self, server_first: &str) -> Result<String, String> {
        let (mut nonce, mut salt, mut iters) = (None, None, None);
        for part in server_first.split(',') {
            match part.split_once('=') {
                Some(("r", v)) => nonce = Some(v),
                Some(("s", v)) => salt = Some(v),
                Some(("i", v)) => iters = Some(v),
                Some(("e", v)) => return Err(format!("the server refused: {v}")),
                Some(("m", _)) => return Err("the server demands an extension this client does not know".into()),
                _ => {}
            }
        }
        let (nonce, salt, iters) = match (nonce, salt, iters) {
            (Some(n), Some(s), Some(i)) => (n, s, i),
            _ => return Err("the server's first message is incomplete".into()),
        };
        // The server must extend OUR nonce; otherwise it could be replaying someone else's exchange.
        if !nonce.starts_with(&self.client_nonce) || nonce.len() <= self.client_nonce.len() {
            return Err("the server's nonce does not extend ours".into());
        }
        let salt = B64.decode(salt).map_err(|_| "the server's salt is not valid base64".to_owned())?;
        let iterations: u32 = iters.parse().map_err(|_| "the server's iteration count is not a number".to_owned())?;
        if !(MIN_ITERATIONS..=MAX_ITERATIONS).contains(&iterations) {
            return Err(format!("the server's iteration count {iterations} is outside {MIN_ITERATIONS}..={MAX_ITERATIONS}"));
        }
        let iterations = NonZeroU32::new(iterations).ok_or("zero iterations")?;

        let mut salted = [0u8; 32];
        pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA256, iterations, &salt, self.password.as_bytes(), &mut salted);
        let hmac_of = |key: &[u8], data: &[u8]| hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data);
        let client_key = hmac_of(&salted, b"Client Key");
        let stored_key = digest::digest(&digest::SHA256, client_key.as_ref());
        let client_final_no_proof = format!("c=biws,r={nonce}");
        let auth_message = format!("{},{server_first},{client_final_no_proof}", self.client_first_bare);
        let client_signature = hmac_of(stored_key.as_ref(), auth_message.as_bytes());
        let proof: Vec<u8> = client_key.as_ref().iter().zip(client_signature.as_ref()).map(|(a, b)| a ^ b).collect();
        let server_key = hmac_of(&salted, b"Server Key");
        let server_signature = hmac_of(server_key.as_ref(), auth_message.as_bytes());
        self.expected_server_signature = Some(server_signature.as_ref().to_vec());
        Ok(format!("{client_final_no_proof},p={}", B64.encode(proof)))
    }

    /// Check the server's final message: it must prove it knows the password too.
    pub fn verify_server_final(&self, server_final: &str) -> Result<(), String> {
        if let Some(e) = server_final.strip_prefix("e=") {
            return Err(format!("the server refused: {e}"));
        }
        let Some(v) = server_final.strip_prefix("v=") else {
            return Err("the server's final message has no signature".into());
        };
        let got = B64.decode(v.split(',').next().unwrap_or("")).map_err(|_| "the server's signature is not valid base64".to_owned())?;
        let want = self.expected_server_signature.as_deref().ok_or("no signature to compare with yet")?;
        if ct_eq(&got, want) {
            Ok(())
        } else {
            Err("the server did not prove it knows the password".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The worked example of RFC 7677, section 3.
    const NONCE: &str = "rOprNGfwEbeRWgbNEkqO";
    const SERVER_FIRST: &str = "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
    const CLIENT_FINAL: &str =
        "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=";
    const SERVER_FINAL: &str = "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=";

    #[test]
    fn matches_the_rfc_7677_test_vector() {
        let mut s = Scram::new("user", "pencil", NONCE);
        assert_eq!(s.client_first(), "n,,n=user,r=rOprNGfwEbeRWgbNEkqO");
        assert_eq!(s.client_final(SERVER_FIRST).unwrap(), CLIENT_FINAL);
        assert!(s.verify_server_final(SERVER_FINAL).is_ok());
    }

    #[test]
    fn a_wrong_password_gives_a_different_proof_and_does_not_verify() {
        let mut s = Scram::new("user", "wrong", NONCE);
        assert_ne!(s.client_final(SERVER_FIRST).unwrap(), CLIENT_FINAL);
        assert!(s.verify_server_final(SERVER_FINAL).is_err(), "the server's signature was made with another password");
    }

    #[test]
    fn a_forged_or_malformed_server_final_is_refused() {
        let mut s = Scram::new("user", "pencil", NONCE);
        assert!(s.verify_server_final(SERVER_FINAL).is_err(), "nothing to compare with before the proof was made");
        s.client_final(SERVER_FIRST).unwrap();
        for bad in [
            "v=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "v=",
            "v=not base64!!",
            "",
            "x=whatever",
            "e=invalid-proof",
            "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4",
        ] {
            assert!(s.verify_server_final(bad).is_err(), "{bad:?}");
        }
        assert!(s.verify_server_final("v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=,extra=ignored").is_ok(), "trailing attributes are allowed");
    }

    #[test]
    fn bad_server_first_messages_are_refused() {
        let go = |sf: &str| Scram::new("user", "pencil", NONCE).client_final(sf);
        assert!(go("r=otherNONCEandmore,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096").unwrap_err().contains("nonce"));
        assert!(go("r=rOprNGfwEbeRWgbNEkqO,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096").unwrap_err().contains("nonce"), "must extend ours");
        assert!(go("r=rOprNGfwEbeRWgbNEkqOxyz,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=100").unwrap_err().contains("iteration"));
        assert!(go("r=rOprNGfwEbeRWgbNEkqOxyz,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=99999999").unwrap_err().contains("iteration"));
        assert!(go("r=rOprNGfwEbeRWgbNEkqOxyz,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=abc").unwrap_err().contains("number"));
        assert!(go("r=rOprNGfwEbeRWgbNEkqOxyz,s=!!!,i=4096").unwrap_err().contains("salt"));
        assert!(go("r=rOprNGfwEbeRWgbNEkqOxyz,i=4096").unwrap_err().contains("incomplete"));
        assert!(go("e=unknown-user").unwrap_err().contains("unknown-user"));
        assert!(go("m=extension,r=rOprNGfwEbeRWgbNEkqOxyz,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096").unwrap_err().contains("extension"));
        assert!(go("").is_err());
    }

    #[test]
    fn special_characters_in_the_user_name_are_escaped() {
        assert_eq!(Scram::new("a=b,c", "pw", "N").client_first(), "n,,n=a=3Db=2Cc,r=N");
    }

    #[test]
    fn only_ascii_credentials_can_use_scram_here() {
        assert!(can_use("alice", "s3cret!"));
        assert!(!can_use("alice", "pässword"));
        assert!(!can_use("ålice", "pw"));
        assert!(!can_use("", "pw"));
    }

    #[test]
    fn nonces_are_long_distinct_and_free_of_commas() {
        let (a, b) = (random_nonce(), random_nonce());
        assert_ne!(a, b);
        assert_eq!(a.len(), 24);
        assert!(!a.contains(','));
    }
}
