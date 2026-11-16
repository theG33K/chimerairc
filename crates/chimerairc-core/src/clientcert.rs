//! A TLS client certificate for SASL EXTERNAL (and trusted extra roots), loaded from PEM files.
//!
//! The certificate and its private key may be in one file (the usual layout for IRC clients) or in
//! two. Encrypted keys are not supported: the error says so rather than failing mysteriously.

use ring::digest;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::path::Path;

pub struct ClientCert {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

impl Clone for ClientCert {
    fn clone(&self) -> Self {
        ClientCert { chain: self.chain.clone(), key: self.key.clone_key() }
    }
}

/// Never prints the key.
impl std::fmt::Debug for ClientCert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClientCert {{ {} certificate(s), fingerprint {}, key hidden }}", self.chain.len(), self.fingerprint())
    }
}

impl ClientCert {
    pub fn from_der(chain: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> Result<ClientCert, String> {
        if chain.is_empty() {
            return Err("no certificate".into());
        }
        Ok(ClientCert { chain, key })
    }

    /// SHA-256 of the first (leaf) certificate as `AB:CD:...`: what services such as NickServ ask for
    /// when registering a certificate.
    pub fn fingerprint(&self) -> String {
        let d = digest::digest(&digest::SHA256, self.chain[0].as_ref());
        d.as_ref().iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
    }

    pub(crate) fn parts(&self) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        (self.chain.clone(), self.key.clone_key())
    }
}

/// Read a certificate chain from `cert_path` and the key from `key_path` (or from `cert_path` when
/// there is none).
pub fn load(cert_path: &Path, key_path: Option<&Path>) -> Result<ClientCert, String> {
    let certs = CertificateDer::pem_file_iter(cert_path)
        .map_err(|e| format!("could not read {}: {e}", cert_path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{} has an unreadable certificate: {e}", cert_path.display()))?;
    if certs.is_empty() {
        return Err(format!("{} contains no certificate (it must be a PEM file)", cert_path.display()));
    }
    let key_file = key_path.unwrap_or(cert_path);
    // an encrypted key is a different PEM label, which the loader would simply not find
    if std::fs::read_to_string(key_file).map(|t| t.contains("ENCRYPTED PRIVATE KEY") || t.contains("Proc-Type: 4,ENCRYPTED")).unwrap_or(false) {
        return Err(format!("the private key in {} is encrypted, which is not supported; use an unencrypted key", key_file.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key_file).map_err(|e| match e {
        rustls::pki_types::pem::Error::NoItemsFound => format!("{} contains no private key", key_file.display()),
        e => format!("could not read the private key in {}: {e}", key_file.display()),
    })?;
    ClientCert::from_der(certs, key)
}

/// Extra trusted root certificates from a PEM file (for a server with its own certificate authority
/// or a self-signed certificate).
pub fn load_roots(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    let certs = CertificateDer::pem_file_iter(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{} has an unreadable certificate: {e}", path.display()))?;
    if certs.is_empty() {
        return Err(format!("{} contains no certificate", path.display()));
    }
    Ok(certs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("chimerairc-cert-test-{}-{tag}", std::process::id()))
    }

    fn generated() -> (String, String) {
        let ck = rcgen::generate_simple_self_signed(vec!["me.example".to_owned()]).unwrap();
        (ck.cert.pem(), ck.signing_key.serialize_pem())
    }

    #[test]
    fn a_combined_pem_file_loads_and_has_a_stable_fingerprint() {
        let (cert, key) = generated();
        let p = temp("combined.pem");
        std::fs::write(&p, format!("{cert}\n{key}")).unwrap();
        let c = load(&p, None).unwrap();
        let fp = c.fingerprint();
        assert_eq!(fp.split(':').count(), 32, "{fp}");
        assert!(fp.chars().all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_lowercase() || ch == ':'), "{fp}");
        assert_eq!(load(&p, None).unwrap().fingerprint(), fp);
        assert_eq!(c.clone().fingerprint(), fp);
        let shown = format!("{c:?}");
        assert!(shown.contains("key hidden") && !shown.contains("PRIVATE"), "{shown}");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn separate_certificate_and_key_files_work() {
        let (cert, key) = generated();
        let (cp, kp) = (temp("c.pem"), temp("k.pem"));
        std::fs::write(&cp, &cert).unwrap();
        std::fs::write(&kp, &key).unwrap();
        assert!(load(&cp, Some(&kp)).is_ok());
        assert!(load(&cp, None).unwrap_err().contains("no private key"), "the key is not in the certificate file");
        let _ = (std::fs::remove_file(cp), std::fs::remove_file(kp));
    }

    #[test]
    fn bad_files_give_clear_errors() {
        let missing = temp("does-not-exist.pem");
        assert!(load(&missing, None).unwrap_err().contains("could not read"));
        let junk = temp("junk.pem");
        std::fs::write(&junk, "this is not pem at all").unwrap();
        assert!(load(&junk, None).unwrap_err().contains("no certificate"));
        let (cert, _) = generated();
        let only_cert = temp("only-cert.pem");
        std::fs::write(&only_cert, &cert).unwrap();
        assert!(load(&only_cert, None).unwrap_err().contains("no private key"));
        let enc = temp("enc.pem");
        std::fs::write(&enc, format!("{cert}\n-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n")).unwrap();
        assert!(load(&enc, None).unwrap_err().contains("encrypted"));
        let _ = (std::fs::remove_file(junk), std::fs::remove_file(only_cert), std::fs::remove_file(enc));
    }

    #[test]
    fn extra_roots_load_and_reject_junk() {
        let (cert, _) = generated();
        let p = temp("root.pem");
        std::fs::write(&p, &cert).unwrap();
        assert_eq!(load_roots(&p).unwrap().len(), 1);
        std::fs::write(&p, "nothing here").unwrap();
        assert!(load_roots(&p).unwrap_err().contains("no certificate"));
        assert!(load_roots(&temp("nope.pem")).is_err());
        let _ = std::fs::remove_file(p);
    }
}
