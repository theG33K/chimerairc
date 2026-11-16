//! TLS for secure DCC (`DCC SSEND` / `DCC SCHAT`, as in mIRC, HexChat and irssi).
//!
//! The peer that *listens* is the TLS server and the peer that *connects* is the TLS client,
//! whichever of them is sending the file. DCC peers have no certificates anyone could check, so:
//! * the server presents a throw-away self-signed certificate made once per run;
//! * the client accepts any certificate (it still checks the handshake signatures, so the key
//!   that signed the handshake really is the key in the certificate).
//!
//! That protects the file or conversation from anyone merely listening in. It does **not**
//! authenticate the other side: someone who can sit in the middle of the connection could still
//! impersonate it, exactly as with plain DCC, and the interface says so.

use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use tokio_rustls::rustls::crypto::{self, CryptoProvider};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use tokio_rustls::rustls::{ClientConfig, DigitallySignedStruct, Error, ServerConfig, SignatureScheme};
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// Any connection a transfer or chat can run over: a plain TCP stream or a TLS stream.
pub trait Conn: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Conn for T {}
pub type BoxConn = Box<dyn Conn>;

/// How long a peer gets to complete the TLS handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

fn provider() -> Arc<CryptoProvider> {
    Arc::new(crypto::ring::default_provider())
}

static SERVER: OnceLock<Result<Arc<ServerConfig>, String>> = OnceLock::new();

fn server_config() -> Result<Arc<ServerConfig>, String> {
    SERVER
        .get_or_init(|| {
            let ck = rcgen::generate_simple_self_signed(vec!["chimerairc-dcc.invalid".to_owned()])
                .map_err(|e| format!("could not make a certificate: {e}"))?;
            let cert: CertificateDer<'static> = ck.cert.der().clone();
            let key = PrivateKeyDer::try_from(ck.signing_key.serialize_der())
                .map_err(|e| format!("could not use the certificate's key: {e}"))?;
            let cfg = ServerConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .map_err(|e| e.to_string())?
                .with_no_client_auth()
                .with_single_cert(vec![cert], key)
                .map_err(|e| e.to_string())?;
            Ok(Arc::new(cfg))
        })
        .clone()
}

/// Accepts whatever certificate the server shows (there is nothing to check it against) but still
/// verifies that the handshake was signed by the key in that certificate.
#[derive(Debug)]
struct AcceptAny(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn client_config() -> Result<Arc<ClientConfig>, String> {
    let p = provider();
    let cfg = ClientConfig::builder_with_provider(p.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(p)))
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

fn describe(version: Option<tokio_rustls::rustls::ProtocolVersion>, suite: Option<tokio_rustls::rustls::SupportedCipherSuite>) -> String {
    let v = version.map(|v| format!("{v:?}")).unwrap_or_else(|| "TLS".into());
    match suite {
        Some(s) => format!("{} ({:?})", v.replace("TLSv1_", "TLS 1."), s.suite()),
        None => v,
    }
}

/// We listen: act as the TLS server. Returns the encrypted stream and a short description.
pub async fn tls_accept(stream: TcpStream) -> Result<(BoxConn, String), String> {
    tls_accept_within(stream, HANDSHAKE_TIMEOUT).await
}

pub async fn tls_accept_within(stream: TcpStream, within: Duration) -> Result<(BoxConn, String), String> {
    let acceptor = TlsAcceptor::from(server_config()?);
    let s = timeout(within, acceptor.accept(stream))
        .await
        .map_err(|_| "the secure (TLS) handshake timed out".to_owned())?
        .map_err(|e| format!("the secure (TLS) handshake failed: {e}"))?;
    let info = describe(s.get_ref().1.protocol_version(), s.get_ref().1.negotiated_cipher_suite());
    Ok((Box::new(s), info))
}

/// We connected: act as the TLS client.
pub async fn tls_connect(stream: TcpStream) -> Result<(BoxConn, String), String> {
    tls_connect_within(stream, HANDSHAKE_TIMEOUT).await
}

pub async fn tls_connect_within(stream: TcpStream, within: Duration) -> Result<(BoxConn, String), String> {
    let connector = TlsConnector::from(client_config()?);
    let name = ServerName::try_from("chimerairc-dcc.invalid").map_err(|e| e.to_string())?;
    let s = timeout(within, connector.connect(name, stream))
        .await
        .map_err(|_| "the secure (TLS) handshake timed out".to_owned())?
        .map_err(|e| format!("the secure (TLS) handshake failed: {e}"))?;
    let info = describe(s.get_ref().1.protocol_version(), s.get_ref().1.negotiated_cipher_suite());
    Ok((Box::new(s), info))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let (c, s) = tokio::join!(TcpStream::connect(addr), l.accept());
        (c.unwrap(), s.unwrap().0)
    }

    #[tokio::test]
    async fn client_and_server_complete_a_handshake_and_exchange_data_both_ways() {
        let (c, s) = pair().await;
        let (client, server) = tokio::join!(tls_connect(c), tls_accept(s));
        let ((mut client, cinfo), (mut server, sinfo)) = (client.unwrap(), server.unwrap());
        assert!(cinfo.starts_with("TLS") && sinfo.starts_with("TLS"), "{cinfo} / {sinfo}");
        client.write_all(b"hello server").await.unwrap();
        client.flush().await.unwrap();
        let mut b = [0u8; 12];
        server.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"hello server");
        server.write_all(b"hi client").await.unwrap();
        server.flush().await.unwrap();
        let mut b = [0u8; 9];
        client.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"hi client");
    }

    #[tokio::test]
    async fn the_data_on_the_wire_is_not_the_plain_text() {
        // a tap between the two ends records everything that crosses it
        let (client_side, tap_a) = pair().await;
        let (tap_b, server_side) = pair().await;
        let seen = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let (mut a_rd, mut a_wr) = tokio::io::split(tap_a);
        let (mut b_rd, mut b_wr) = tokio::io::split(tap_b);
        let (s1, s2) = (seen.clone(), seen.clone());
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = a_rd.read(&mut buf).await {
                if n == 0 || b_wr.write_all(&buf[..n]).await.is_err() {
                    break;
                }
                s1.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = b_rd.read(&mut buf).await {
                if n == 0 || a_wr.write_all(&buf[..n]).await.is_err() {
                    break;
                }
                s2.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        let (client, server) = tokio::join!(tls_connect(client_side), tls_accept(server_side));
        let (mut client, _) = client.unwrap();
        let (mut server, _) = server.unwrap();
        let secret = b"the quick brown fox jumps over the lazy dog, secretly";
        client.write_all(secret).await.unwrap();
        client.flush().await.unwrap();
        let mut got = vec![0u8; secret.len()];
        server.read_exact(&mut got).await.unwrap();
        assert_eq!(&got[..], &secret[..]);
        let wire = seen.lock().unwrap().clone();
        assert!(!wire.is_empty());
        assert!(!wire.windows(secret.len()).any(|w| w == secret), "the plain text must not appear on the wire");
    }

    #[tokio::test]
    async fn plain_text_to_a_tls_server_is_rejected_and_a_silent_peer_times_out() {
        let (mut c, s) = pair().await;
        c.write_all(b"GET / HTTP/1.0\r\n\r\nthis is not a TLS hello\r\n").await.unwrap();
        let e = tls_accept(s).await.err().expect("plain text must not pass as TLS");
        assert!(e.contains("handshake failed"), "{e}");
        let (_quiet, s) = pair().await;
        let e = tls_accept_within(s, Duration::from_millis(300)).await.err().unwrap();
        assert!(e.contains("timed out"), "{e}");
    }

    #[tokio::test]
    async fn a_tls_client_gives_up_on_a_server_that_does_not_speak_tls() {
        let (c, mut s) = pair().await;
        tokio::spawn(async move {
            let _ = s.write_all(b"220 hello, I am a plain text server\r\n").await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let r = tls_connect_within(c, Duration::from_millis(800)).await;
        assert!(r.is_err());
    }
}
