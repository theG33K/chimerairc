//! End-to-end: real tokio sockets against a scripted plaintext server.

use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::time::timeout;
use chimerairc_core::{net, Config, Event};

const T: Duration = Duration::from_secs(5);

async fn read<R: tokio::io::AsyncBufRead + Unpin>(rd: &mut R, line: &mut String) {
    line.clear();
    timeout(T, rd.read_line(line)).await.expect("server read timeout").unwrap();
}

async fn next_event(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>) -> Event {
    loop {
        let e = timeout(T, rx.recv()).await.expect("event timeout").expect("channel closed");
        if !matches!(e, Event::Numeric { .. }) {
            return e;
        }
    }
}

#[tokio::test]
async fn full_session_with_cap_sasl_chat_and_quit() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let (rd, mut wr) = sock.into_split();
        let mut rd = BufReader::new(rd);
        let mut line = String::new();
        // On a plaintext connection with a login, nothing but CAP LS goes out until the server has
        // answered it (it might demand TLS); registration follows the answer.
        read(&mut rd, &mut line).await;
        assert_eq!(line.trim_end(), "CAP LS 302");
        let mut quiet = String::new();
        let waited = timeout(Duration::from_millis(300), rd.read_line(&mut quiet)).await;
        assert!(waited.is_err(), "the client sent {quiet:?} before the server answered CAP LS");
        wr.write_all(b":srv CAP * LS :multi-prefix sasl=PLAIN unknown\r\n").await.unwrap();
        for want in ["NICK tester", "USER tester 0 * :Test Er", "CAP REQ multi-prefix", "CAP REQ sasl"] {
            read(&mut rd, &mut line).await;
            assert_eq!(line.trim_end(), want);
        }
        wr.write_all(b":srv CAP tester ACK multi-prefix\r\n:srv CAP tester ACK sasl\r\n").await.unwrap();
        read(&mut rd, &mut line).await;
        assert_eq!(line.trim_end(), "AUTHENTICATE PLAIN");
        wr.write_all(b"AUTHENTICATE +\r\n").await.unwrap();
        read(&mut rd, &mut line).await;
        assert_eq!(line.trim_end(), "AUTHENTICATE AGFjY3QAcHc=");
        wr.write_all(b":srv 903 tester :SASL successful\r\n").await.unwrap();
        read(&mut rd, &mut line).await;
        assert_eq!(line.trim_end(), "CAP END");
        wr.write_all(
            b":srv 001 tester :Welcome\r\nPING :abc\r\n:bob!b@h PRIVMSG tester :hi there\r\n",
        )
        .await
        .unwrap();
        read(&mut rd, &mut line).await;
        assert_eq!(line.trim_end(), "PONG abc");
        // Client-originated: a message trying to inject a second command must be refused.
        read(&mut rd, &mut line).await;
        assert_eq!(line.trim_end(), "PRIVMSG bob ok");
        read(&mut rd, &mut line).await;
        assert!(line.starts_with("QUIT"), "got {line:?}");
        // hang up
    });

    let mut cfg = Config::new("127.0.0.1", "tester");
    cfg.port = port;
    cfg.tls = false;
    cfg.realname = "Test Er".into();
    cfg.sasl = Some(("acct".into(), "pw".into()));
    let (client, mut rx) = net::spawn(cfg);

    assert_eq!(next_event(&mut rx).await, Event::Connected);
    assert!(matches!(next_event(&mut rx).await, Event::SaslResult { ok: true, .. }));
    assert!(matches!(next_event(&mut rx).await, Event::CapsEnabled { .. }));
    assert!(matches!(next_event(&mut rx).await, Event::Registered { nick } if nick == "tester"));
    assert!(matches!(next_event(&mut rx).await, Event::Chat { text, .. } if text == "hi there"));

    assert!(client.privmsg("bob", "evil\r\nQUIT :pwned"));
    assert!(matches!(next_event(&mut rx).await, Event::Error { .. }));
    assert!(client.privmsg("bob", "ok"));
    client.quit(Some("bye"));
    assert!(matches!(next_event(&mut rx).await, Event::Disconnected { .. }));
    server.await.unwrap();
}

#[tokio::test]
async fn connection_refused_reports_disconnect() {
    // Bind then drop to get a port that is very likely closed.
    let port = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
    let mut cfg = Config::new("127.0.0.1", "x");
    cfg.port = port;
    cfg.tls = false;
    let (_c, mut rx) = net::spawn(cfg);
    assert!(matches!(next_event(&mut rx).await, Event::Disconnected { .. }));
}

#[tokio::test]
async fn conn_info_reports_both_ends_once_connected() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (_sock, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await; // keep it open while the test looks
    });
    let mut cfg = Config::new("127.0.0.1", "x");
    cfg.port = port;
    cfg.tls = false;
    let (client, mut rx) = net::spawn(cfg);
    assert_eq!(client.conn_info(), net::ConnInfo::default(), "nothing known before connecting");
    assert_eq!(next_event(&mut rx).await, Event::Connected);
    let info = client.conn_info();
    let loopback: std::net::IpAddr = "127.0.0.1".parse().unwrap();
    assert_eq!((info.local, info.server), (Some(loopback), Some(loopback)));
}
