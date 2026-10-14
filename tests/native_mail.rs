//! Native direct delivery against isolated loopback SMTP servers with real
//! STARTTLS (fixture CA), explicit fixture DNS, adversarial replies, failover,
//! ambiguous acceptance and independent DKIM verification in Node.
use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use playscale::mail::{self, Content, Direct, Fixture, dns};
use playscale_core::mail::{Observation, Stage};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::Mutex,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        self, RootCertStore,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    },
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/mail");
const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

fn fixture_file(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURES}/{name}")).unwrap()
}

#[derive(Clone)]
struct Behavior {
    greeting: &'static str,
    starttls: bool,
    /// Bytes written immediately after the STARTTLS 220 (injection test).
    after_starttls: &'static str,
    /// Reply to STARTTLS; only "220" proceeds to the handshake.
    starttls_reply: &'static str,
    rcpt: &'static str,
    /// None: close the connection after the terminator without replying.
    final_reply: Option<&'static str>,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            greeting: "220 mx.fixture.test ESMTP\r\n",
            starttls: true,
            after_starttls: "",
            starttls_reply: "220 go ahead\r\n",
            rcpt: "250 ok\r\n",
            final_reply: Some("250 queued\r\n"),
        }
    }
}

#[derive(Default)]
struct Log {
    connections: AtomicUsize,
    plaintext_mail: AtomicUsize,
    data_commands: AtomicUsize,
    messages: Mutex<Vec<Vec<u8>>>,
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

async fn server(behavior: Behavior) -> (u16, Arc<Log>) {
    let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Log::default());
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from_pem_slice(&fixture_file("mx.pem")).unwrap()],
            PrivateKeyDer::from_pem_slice(&fixture_file("mx.key")).unwrap(),
        )
        .unwrap(),
    );
    let shared = log.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let (log, behavior, config) = (shared.clone(), behavior.clone(), config.clone());
            tokio::spawn(async move {
                log.connections.fetch_add(1, Ordering::SeqCst);
                let _ = session(Box::new(tcp), behavior, config, log).await;
            });
        }
    });
    (port, log)
}

async fn session(
    stream: Box<dyn Stream>,
    behavior: Behavior,
    config: Arc<rustls::ServerConfig>,
    log: Arc<Log>,
) -> std::io::Result<()> {
    let mut io = BufReader::new(stream);
    io.get_mut().write_all(behavior.greeting.as_bytes()).await?;
    if !behavior.greeting.starts_with("220") {
        return Ok(());
    }
    let mut tls = false;
    let mut data = false;
    let mut message = Vec::new();
    loop {
        let mut line = Vec::new();
        if io.read_until(b'\n', &mut line).await? == 0 {
            return Ok(());
        }
        if data {
            if line == b".\r\n" {
                data = false;
                // Undo dot-stuffing exactly as a receiver does.
                let mut clean = Vec::new();
                for part in message.split_inclusive(|b| *b == b'\n') {
                    clean.extend_from_slice(part.strip_prefix(b".").unwrap_or(part));
                }
                log.messages.lock().await.push(clean);
                message.clear();
                match behavior.final_reply {
                    Some(reply) => io.get_mut().write_all(reply.as_bytes()).await?,
                    None => return Ok(()),
                }
            } else {
                message.extend_from_slice(&line);
            }
            continue;
        }
        let text = String::from_utf8_lossy(&line).to_ascii_uppercase();
        let reply: String = if text.starts_with("EHLO") {
            if behavior.starttls && !tls {
                "250-mx.fixture.test\r\n250-PIPELINING\r\n250 STARTTLS\r\n".into()
            } else {
                "250-mx.fixture.test\r\n250 8BITMIME\r\n".into()
            }
        } else if text.starts_with("STARTTLS") && behavior.starttls && !tls {
            io.get_mut()
                .write_all(
                    format!("{}{}", behavior.starttls_reply, behavior.after_starttls).as_bytes(),
                )
                .await?;
            if !behavior.starttls_reply.starts_with("220") {
                continue;
            }
            let inner = io.into_inner();
            let accepted = TlsAcceptor::from(config.clone()).accept(inner).await?;
            io = BufReader::new(Box::new(accepted) as Box<dyn Stream>);
            tls = true;
            continue;
        } else if text.starts_with("MAIL FROM") {
            if !tls {
                log.plaintext_mail.fetch_add(1, Ordering::SeqCst);
            }
            "250 sender ok\r\n".into()
        } else if text.starts_with("RCPT TO") {
            behavior.rcpt.into()
        } else if text.starts_with("DATA") {
            log.data_commands.fetch_add(1, Ordering::SeqCst);
            data = true;
            "354 go\r\n".into()
        } else if text.starts_with("QUIT") {
            io.get_mut().write_all(b"221 bye\r\n").await?;
            return Ok(());
        } else {
            "502 unsupported\r\n".into()
        };
        io.get_mut().write_all(reply.as_bytes()).await?;
    }
}

fn roots() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(&fixture_file("ca.pem")).unwrap())
        .unwrap();
    roots
}

/// Fixture DNS: recipient domain -> MX list; each exchanger -> addresses.
fn routing(
    domain: &str,
    mx: dns::Answer<Vec<(u16, String)>>,
    hosts: &[(&str, dns::Answer<Vec<IpAddr>>, u16)],
) -> mail::Routing {
    let runtime = mail::Runtime::default();
    let mut fixture = dns::Fixture::default();
    fixture.mx.insert(domain.into(), mx);
    let mut ports = BTreeMap::new();
    for (host, answer, port) in hosts {
        fixture.addresses.insert((*host).into(), answer.clone());
        ports.insert((*host).into(), *port);
    }
    runtime.use_fixture(Fixture {
        dns: fixture,
        roots: roots(),
        ports,
    });
    runtime.routing()
}

fn signed_message(code: &str) -> Vec<u8> {
    let key = mail::dkim::import(std::str::from_utf8(&fixture_file("dkim.pem")).unwrap()).unwrap();
    let from = mail::message::parse_mailbox("Motion <motion@sender.test>").unwrap();
    mail::render(
        &Content::SigninCode { code },
        &from,
        "person@example.test",
        "message-1",
        "mail.sender.test",
        1_791_990_000,
        Some(mail::Signing {
            selector: "motion1",
            pkcs8: &key.pkcs8,
        }),
    )
    .unwrap()
    .bytes
}

async fn send(routing: &mail::Routing, message: &[u8], recheck: bool) -> (Observation, String) {
    mail::deliver_direct(
        routing,
        &Direct {
            helo: "mail.sender.test",
            envelope_from: "bounces@sender.test",
            recipient: "person@example.test",
            message,
            loopback_port: None,
        },
        || async move { recheck },
    )
    .await
}

fn one(host: &str) -> dns::Answer<Vec<(u16, String)>> {
    dns::Answer::Records(vec![(10, host.into())])
}
fn local() -> dns::Answer<Vec<IpAddr>> {
    dns::Answer::Records(vec![LOCAL])
}

fn dkim_verify(message: &[u8]) -> serde_json::Value {
    let key = mail::dkim::import(std::str::from_utf8(&fixture_file("dkim.pem")).unwrap()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("message.eml");
    std::fs::write(&path, message).unwrap();
    let output = std::process::Command::new("node")
        .arg(format!("{FIXTURES}/dkim_verify.mjs"))
        .arg(&path)
        .arg(mail::dkim::dns_record(&key.public))
        .output()
        .expect("Node is required for the independent DKIM verifier");
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)))
}

#[tokio::test]
async fn starttls_delivery_transmits_exact_dkim_verified_bytes() {
    let (port, log) = server(Behavior::default()).await;
    let routing = routing(
        "example.test",
        one("mx1.fixture.test"),
        &[("mx1.fixture.test", local(), port)],
    );
    let message = signed_message("12345678");
    let (observation, detail) = send(&routing, &message, true).await;
    assert_eq!(observation, Observation::Accepted, "{detail}");
    assert_eq!(log.plaintext_mail.load(Ordering::SeqCst), 0);
    let received = log.messages.lock().await[0].clone();
    assert_eq!(
        received, message,
        "transmitted bytes differ from signed bytes"
    );
    let verified = dkim_verify(&received);
    assert_eq!(verified["ok"], true, "{verified}");
    assert_eq!(verified["d"], "sender.test");
    assert_eq!(verified["s"], "motion1");
}

#[tokio::test]
async fn independent_verifier_rejects_tampered_and_added_headers() {
    let message = signed_message("12345678");
    assert_eq!(dkim_verify(&message)["ok"], true);
    let text = String::from_utf8(message.clone()).unwrap();
    let tampered = text.replace("12345678", "87654321");
    assert_eq!(dkim_verify(tampered.as_bytes())["ok"], false);
    let renamed = text.replace("Subject: Your Motion", "Subject: Your  Motion!");
    assert_eq!(dkim_verify(renamed.as_bytes())["ok"], false);
    // Over-signed From: a second From added in transit breaks the signature.
    let added = text.replacen("Date:", "From: <attacker@evil.test>\r\nDate:", 1);
    assert_eq!(dkim_verify(added.as_bytes())["ok"], false);
    // Relaxed canonicalization tolerates refolding of a signed header.
    let refolded = text.replacen("Subject: Your Motion", "Subject: Your\r\n   Motion", 1);
    assert_eq!(dkim_verify(refolded.as_bytes())["ok"], true);
}

#[tokio::test]
async fn tls_policy_never_downgrades_to_plaintext() {
    let message = signed_message("12345678");
    // STARTTLS not offered by any exchanger: permanent TLS policy failure.
    let (port, log) = server(Behavior {
        starttls: false,
        ..Default::default()
    })
    .await;
    let r = routing(
        "example.test",
        dns::Answer::Records(vec![
            (10, "mx1.fixture.test".into()),
            (20, "mx2.fixture.test".into()),
        ]),
        &[
            ("mx1.fixture.test", local(), port),
            ("mx2.fixture.test", local(), port),
        ],
    );
    assert_eq!(
        send(&r, &message, true).await.0,
        Observation::Permanent(Stage::TlsPolicy)
    );
    assert_eq!(log.connections.load(Ordering::SeqCst), 2);
    assert_eq!(log.plaintext_mail.load(Ordering::SeqCst), 0);
    // Certificate not valid for the exchanger name.
    let (port, log) = server(Behavior::default()).await;
    let r = routing(
        "example.test",
        one("mx9.other.test"),
        &[("mx9.other.test", local(), port)],
    );
    assert_eq!(
        send(&r, &message, true).await.0,
        Observation::Permanent(Stage::TlsPolicy)
    );
    assert_eq!(log.plaintext_mail.load(Ordering::SeqCst), 0);
    assert!(log.messages.lock().await.is_empty());
    // A transient 454 to STARTTLS stays temporary and is retried later.
    let (port, _) = server(Behavior {
        starttls_reply: "454 TLS not available now\r\n",
        ..Default::default()
    })
    .await;
    let r = routing(
        "example.test",
        one("mx1.fixture.test"),
        &[("mx1.fixture.test", local(), port)],
    );
    assert_eq!(
        send(&r, &message, true).await.0,
        Observation::Temporary(Stage::Tls)
    );
    // Plaintext bytes injected after the STARTTLS reply are refused.
    let (port, log) = server(Behavior {
        after_starttls: "250 injected\r\n",
        ..Default::default()
    })
    .await;
    let r = routing(
        "example.test",
        one("mx1.fixture.test"),
        &[("mx1.fixture.test", local(), port)],
    );
    assert_eq!(
        send(&r, &message, true).await.0,
        Observation::Permanent(Stage::TlsPolicy)
    );
    assert!(log.messages.lock().await.is_empty());
}

#[tokio::test]
async fn failover_classification_and_ambiguous_acceptance() {
    let message = signed_message("12345678");
    // Temporary greeting refusal at the preferred exchanger fails over.
    let (busy, busy_log) = server(Behavior {
        greeting: "421 busy\r\n",
        ..Default::default()
    })
    .await;
    let (good, good_log) = server(Behavior::default()).await;
    let r = routing(
        "example.test",
        dns::Answer::Records(vec![
            (20, "mx2.fixture.test".into()),
            (10, "mx1.fixture.test".into()),
        ]),
        &[
            ("mx1.fixture.test", local(), busy),
            ("mx2.fixture.test", local(), good),
        ],
    );
    let (observation, detail) = send(&r, &message, true).await;
    assert_eq!(observation, Observation::Accepted, "{detail}");
    assert_eq!(busy_log.connections.load(Ordering::SeqCst), 1);
    assert_eq!(good_log.messages.lock().await.len(), 1);

    // A permanent RCPT refusal is authoritative: no failover, no DATA.
    let (refusing, refusing_log) = server(Behavior {
        rcpt: "550 5.1.1 no such user\r\n",
        ..Default::default()
    })
    .await;
    let (other, other_log) = server(Behavior::default()).await;
    let r = routing(
        "example.test",
        dns::Answer::Records(vec![
            (10, "mx1.fixture.test".into()),
            (20, "mx2.fixture.test".into()),
        ]),
        &[
            ("mx1.fixture.test", local(), refusing),
            ("mx2.fixture.test", local(), other),
        ],
    );
    let (observation, detail) = send(&r, &message, true).await;
    assert_eq!(observation, Observation::Permanent(Stage::RcptTo));
    assert!(
        !detail.contains('@'),
        "diagnostics must not include addresses: {detail}"
    );
    assert_eq!(refusing_log.data_commands.load(Ordering::SeqCst), 0);
    assert_eq!(other_log.connections.load(Ordering::SeqCst), 0);

    // No reply after the final terminator: acceptance is unknown, not failed.
    let (silent, silent_log) = server(Behavior {
        final_reply: None,
        ..Default::default()
    })
    .await;
    let r = routing(
        "example.test",
        one("mx1.fixture.test"),
        &[("mx1.fixture.test", local(), silent)],
    );
    assert_eq!(send(&r, &message, true).await.0, Observation::Ambiguous);
    assert_eq!(silent_log.messages.lock().await.len(), 1);

    // A 4xx after the terminator is a temporary refusal of the message.
    let (deferring, _) = server(Behavior {
        final_reply: Some("451 try later\r\n"),
        ..Default::default()
    })
    .await;
    let r = routing(
        "example.test",
        one("mx1.fixture.test"),
        &[("mx1.fixture.test", local(), deferring)],
    );
    assert_eq!(
        send(&r, &message, true).await.0,
        Observation::Temporary(Stage::Message)
    );

    // Eligibility withdrawn before DATA: no secret content is transmitted.
    let (port, log) = server(Behavior::default()).await;
    let r = routing(
        "example.test",
        one("mx1.fixture.test"),
        &[("mx1.fixture.test", local(), port)],
    );
    assert_eq!(send(&r, &message, false).await.0, Observation::Withdrawn);
    assert_eq!(log.data_commands.load(Ordering::SeqCst), 0);
    assert!(log.messages.lock().await.is_empty());

    // Malformed or oversized replies are bounded temporary failures.
    let (garbled, _) = server(Behavior {
        greeting: "220-ok\r\n221 mixed codes\r\n",
        ..Default::default()
    })
    .await;
    let r = routing(
        "example.test",
        one("mx1.fixture.test"),
        &[("mx1.fixture.test", local(), garbled)],
    );
    assert_eq!(
        send(&r, &message, true).await.0,
        Observation::Temporary(Stage::Greeting)
    );
}

#[tokio::test]
async fn dns_routing_distinguishes_null_nx_temporary_implicit_and_disallowed() {
    let message = signed_message("12345678");
    let r = routing("example.test", one("."), &[]);
    assert_eq!(
        send(&r, &message, true).await.0,
        Observation::Permanent(Stage::Route)
    );
    let r = routing("other.test", one("mx1.fixture.test"), &[]);
    assert_eq!(
        send(&r, &message, true).await.0,
        Observation::Permanent(Stage::Dns)
    );
    let r = routing("example.test", dns::Answer::Temporary, &[]);
    assert_eq!(
        send(&r, &message, true).await.0,
        Observation::Temporary(Stage::Dns)
    );
    // Exchanger address lookup failing temporarily is temporary.
    let r = routing(
        "example.test",
        one("mx1.fixture.test"),
        &[("mx1.fixture.test", dns::Answer::Temporary, 25)],
    );
    assert_eq!(
        send(&r, &message, true).await.0,
        Observation::Temporary(Stage::Dns)
    );
    // No MX records: the domain itself is the implicit exchanger (RFC 5321 §5.1).
    let (port, log) = server(Behavior::default()).await;
    let mut fixture = dns::Fixture::default();
    fixture
        .mx
        .insert("fixture.test".into(), dns::Answer::NoRecords);
    fixture.addresses.insert("fixture.test".into(), local());
    let runtime = mail::Runtime::default();
    runtime.use_fixture(Fixture {
        dns: fixture,
        roots: roots(),
        ports: BTreeMap::from([("fixture.test".to_owned(), port)]),
    });
    let routes = mail::routes(&runtime.routing(), "fixture.test")
        .await
        .unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].host, "fixture.test");
    let (observation, _) = mail::deliver_direct(
        &runtime.routing(),
        &Direct {
            helo: "mail.sender.test",
            envelope_from: "bounces@sender.test",
            recipient: "person@fixture.test",
            message: &message,
            loopback_port: None,
        },
        || async { true },
    )
    .await;
    assert_eq!(observation, Observation::Accepted);
    assert_eq!(log.messages.lock().await.len(), 1);
    // Private, link-local, mapped and documentation destinations are never
    // contacted, even with the loopback fixture exception enabled.
    for address in [
        "10.0.0.25",
        "169.254.169.254",
        "192.168.1.1",
        "::ffff:127.0.0.1",
        "fe80::1",
        "fc00::1",
        "2001:db8::1",
        "0.0.0.0",
    ] {
        let r = routing(
            "example.test",
            one("mx1.fixture.test"),
            &[(
                "mx1.fixture.test",
                dns::Answer::Records(vec![address.parse().unwrap()]),
                25,
            )],
        );
        assert_eq!(
            send(&r, &message, true).await.0,
            Observation::Permanent(Stage::Route),
            "{address}"
        );
    }
    // Production routing (no fixture) refuses loopback destinations too.
    let production = mail::Runtime::default().routing();
    assert!(production.fixture_ports.is_none());
}

#[test]
fn imported_keys_are_bounded_and_identified() {
    let pem = String::from_utf8(fixture_file("dkim.pem")).unwrap();
    let key = mail::dkim::import(&pem).unwrap();
    assert_eq!(mail::dkim::key_id(&key.public).len(), 16);
    assert!(mail::dkim::dns_record(&key.public).starts_with("v=DKIM1; k=rsa; p=MII"));
    assert!(
        mail::dkim::import("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----").is_err()
    );
    assert!(mail::dkim::import(&"x".repeat(20_000)).is_err());
}
