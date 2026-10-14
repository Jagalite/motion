//! Transactional mail adapters: message composition, DKIM signing, SMTP relay
//! submission and native direct delivery to recipient mail exchangers.
//!
//! These functions perform bounded I/O and report explicit observations. They
//! never decide retries, cancellation or account state; `playscale_core::mail`
//! does, under the durable outbox in `v2::signin`.
pub mod diagnostics;
pub mod dkim;
pub mod dns;
pub mod message;
pub mod smtp;

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, anyhow, ensure};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use playscale_core::mail::{Observation, Stage, combine_routes, try_next_route};
use tokio_rustls::rustls::RootCertStore;

pub use message::Mailbox;

/// Direct delivery starts no new route after this much of an attempt elapsed.
pub const ROUTE_CUTOFF: Duration = Duration::from_secs(240);
pub const MAX_ROUTES: usize = 8;

/// Test/qualification injection point for DNS, trust anchors and loopback
/// routes. Production uses the system resolver, the WebPKI roots and public
/// unicast destinations on port 25 only.
#[derive(Clone)]
pub struct Fixture {
    pub dns: dns::Fixture,
    pub roots: RootCertStore,
    /// Loopback destinations become allowed; each exchanger host connects to
    /// its listed port (25 when absent).
    pub ports: std::collections::BTreeMap<String, u16>,
}

#[derive(Default)]
pub struct Runtime {
    fixture: Mutex<Option<Arc<Fixture>>>,
    resolver: dns::Resolver,
}

#[derive(Clone)]
pub struct Routing {
    pub resolver: dns::Resolver,
    pub tls: smtp::Tls,
    /// Present only for an explicit fixture: loopback allowed, per-host ports.
    pub fixture_ports: Option<std::collections::BTreeMap<String, u16>>,
}

impl Runtime {
    /// Replace DNS, trust anchors and destination policy with an explicit
    /// isolated fixture. Intended for tests and loopback qualification only.
    pub fn use_fixture(&self, fixture: Fixture) {
        *self.fixture.lock().unwrap() = Some(Arc::new(fixture));
    }
    pub fn routing(&self) -> Routing {
        match self.fixture.lock().unwrap().clone() {
            Some(fixture) => Routing {
                resolver: dns::Resolver::Fixture(Arc::new(fixture.dns.clone())),
                tls: smtp::Tls::Required(Arc::new(fixture.roots.clone())),
                fixture_ports: Some(fixture.ports.clone()),
            },
            None => Routing {
                resolver: self.resolver.clone(),
                tls: smtp::Tls::Required(Arc::new(RootCertStore::from_iter(
                    webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
                ))),
                fixture_ports: None,
            },
        }
    }
}

impl Routing {
    fn allowed(&self, address: IpAddr) -> bool {
        if self.fixture_ports.is_some() && address.is_loopback() {
            return true;
        }
        crate::signin_protocol::is_public_address(address)
    }
    fn port(&self, host: &str) -> u16 {
        self.fixture_ports
            .as_ref()
            .and_then(|ports| ports.get(host).copied())
            .unwrap_or(25)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub host: String,
    pub address: SocketAddr,
}

/// Resolve the delivery routes for `domain` in preference order, pinning each
/// checked address to the socket destination actually used.
pub async fn routes(routing: &Routing, domain: &str) -> Result<Vec<Route>, (Observation, String)> {
    let domain = dns::host_name(domain).ok_or((
        Observation::Permanent(Stage::Dns),
        "invalid domain".to_owned(),
    ))?;
    let exchanges = match routing.resolver.mx(&domain).await {
        dns::Mx::Exchanges(exchanges) => exchanges,
        dns::Mx::None => vec![(0, domain.clone())],
        dns::Mx::Null => {
            return Err((
                Observation::Permanent(Stage::Route),
                "domain accepts no mail (null mx)".into(),
            ));
        }
        dns::Mx::NxDomain => {
            return Err((
                Observation::Permanent(Stage::Dns),
                "domain does not exist".into(),
            ));
        }
        dns::Mx::Temporary => {
            return Err((
                Observation::Temporary(Stage::Dns),
                "mx lookup failed temporarily".into(),
            ));
        }
    };
    let (mut routes, mut temporary, mut disallowed) = (Vec::new(), false, false);
    for (_, host) in exchanges {
        match routing.resolver.addresses(&host).await {
            dns::Answer::Records(addresses) => {
                for address in addresses {
                    if !routing.allowed(address) {
                        disallowed = true;
                        continue;
                    }
                    if routes.len() < MAX_ROUTES {
                        routes.push(Route {
                            host: host.clone(),
                            address: SocketAddr::new(address, routing.port(&host)),
                        });
                    }
                }
            }
            dns::Answer::Temporary => temporary = true,
            dns::Answer::NoRecords | dns::Answer::NxDomain => {}
        }
    }
    if routes.is_empty() {
        return Err(if temporary {
            (
                Observation::Temporary(Stage::Dns),
                "exchanger address lookup failed temporarily".into(),
            )
        } else if disallowed {
            (
                Observation::Permanent(Stage::Route),
                "exchangers resolve only to disallowed addresses".into(),
            )
        } else {
            (
                Observation::Permanent(Stage::Dns),
                "exchangers have no addresses".into(),
            )
        });
    }
    Ok(routes)
}

pub struct Direct<'a> {
    pub helo: &'a str,
    pub envelope_from: &'a str,
    pub recipient: &'a str,
    pub message: &'a [u8],
    /// Explicit isolated qualification route: 127.0.0.1:port, plaintext, no DNS.
    pub loopback_port: Option<u16>,
}

/// Deliver directly to the recipient domain's exchangers. `recheck` is
/// evaluated on each route immediately before DATA.
pub async fn deliver_direct<F, Fut>(
    routing: &Routing,
    direct: &Direct<'_>,
    recheck: F,
) -> (Observation, String)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let (routes, tls) = match direct.loopback_port {
        Some(port) => (
            vec![Route {
                host: "localhost".into(),
                address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            }],
            smtp::Tls::LoopbackPlaintext,
        ),
        None => {
            let Some((_, domain)) = direct.recipient.rsplit_once('@') else {
                return (
                    Observation::Permanent(Stage::Dns),
                    "invalid recipient".into(),
                );
            };
            match routes(routing, domain).await {
                Ok(routes) => (routes, routing.tls.clone()),
                Err(failure) => return failure,
            }
        }
    };
    let started = tokio::time::Instant::now();
    let mut outcomes = Vec::new();
    let mut details = Vec::new();
    for route in &routes {
        if started.elapsed() >= ROUTE_CUTOFF {
            break;
        }
        let result = smtp::deliver(
            &smtp::Session {
                route_host: &route.host,
                address: route.address,
                helo: direct.helo,
                mail_from: direct.envelope_from,
                rcpt_to: direct.recipient,
                tls: &tls,
                message: direct.message,
            },
            recheck(),
        )
        .await;
        details.push(format!("{}: {}", route.host, result.detail));
        outcomes.push(result.observation);
        if !try_next_route(result.observation) {
            break;
        }
    }
    (combine_routes(&outcomes), details.join("; "))
}

#[derive(Clone)]
pub struct Relay {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Plain SMTP is allowed only to a literal loopback address, never a name
    /// that could resolve differently after this configuration is validated.
    pub loopback_test: bool,
}

pub fn validate_relay(config: &Relay) -> Result<()> {
    ensure!(
        !config.host.is_empty()
            && config.host.len() <= 253
            && !config
                .host
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || b"/@?#\\".contains(&byte))
            && config.port != 0,
        "invalid SMTP destination"
    );
    ensure!(
        config.username.is_some() == config.password.is_some(),
        "SMTP username and password must be configured together"
    );
    if config.loopback_test {
        ensure!(
            config
                .host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback()),
            "plain SMTP requires a literal loopback test address"
        );
    }
    Ok(())
}

/// Submit to the configured relay. lettre does not expose the SMTP stage of a
/// network failure, so the connection is tested first: a failure there cannot
/// have delivered anything, while a later transport failure is ambiguous.
pub async fn deliver_relay<Fut>(
    config: &Relay,
    envelope_from: &str,
    recipient: &str,
    message: &[u8],
    recheck: Fut,
) -> (Observation, String)
where
    Fut: std::future::Future<Output = bool>,
{
    if validate_relay(config).is_err() {
        return (
            Observation::Permanent(Stage::Route),
            "invalid relay configuration".into(),
        );
    }
    let builder = if config.loopback_test {
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host)
    } else {
        match AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host) {
            Ok(builder) => builder,
            Err(_) => {
                return (
                    Observation::Permanent(Stage::Tls),
                    "relay tls configuration".into(),
                );
            }
        }
    };
    let mut builder = builder
        .port(config.port)
        .timeout(Some(Duration::from_secs(10)));
    if let (Some(username), Some(password)) = (&config.username, &config.password) {
        builder = builder.credentials(lettre::transport::smtp::authentication::Credentials::new(
            username.clone(),
            password.clone(),
        ));
    }
    let transport = builder.build();
    match tokio::time::timeout(Duration::from_secs(15), transport.test_connection()).await {
        Ok(Ok(true)) => {}
        _ => {
            return (
                Observation::Temporary(Stage::Connect),
                "relay unavailable".into(),
            );
        }
    }
    if !recheck.await {
        return (Observation::Withdrawn, "withdrawn before submission".into());
    }
    let (Ok(from), Ok(to)) = (
        envelope_from.parse::<lettre::Address>(),
        recipient.parse::<lettre::Address>(),
    ) else {
        return (
            Observation::Permanent(Stage::MailFrom),
            "invalid envelope".into(),
        );
    };
    let Ok(envelope) = lettre::address::Envelope::new(Some(from), vec![to]) else {
        return (
            Observation::Permanent(Stage::MailFrom),
            "invalid envelope".into(),
        );
    };
    match tokio::time::timeout(
        Duration::from_secs(45),
        transport.send_raw(&envelope, message),
    )
    .await
    {
        Ok(Ok(_)) => (Observation::Accepted, "accepted by relay".into()),
        Ok(Err(error)) if error.is_permanent() => (
            Observation::Permanent(Stage::Message),
            format!(
                "relay refused {}",
                error.status().map(|c| c.to_string()).unwrap_or_default()
            ),
        ),
        Ok(Err(error)) if error.is_transient() => (
            Observation::Temporary(Stage::Message),
            format!(
                "relay deferred {}",
                error.status().map(|c| c.to_string()).unwrap_or_default()
            ),
        ),
        Ok(Err(error)) if error.is_tls() => (
            Observation::Temporary(Stage::Tls),
            "relay tls failed".into(),
        ),
        _ => (
            Observation::Ambiguous,
            "relay connection ended without a final reply".into(),
        ),
    }
}

/// What a message says. Codes are generated by the caller and never logged.
pub enum Content<'a> {
    SigninCode { code: &'a str },
    RegistrationCode { code: &'a str, origin: &'a str },
    Invitation { origin: &'a str, expires_at: i64 },
    AccountNotice { origin: &'a str },
    Test { code: &'a str, origin: &'a str },
}

fn code_ok(code: &str) -> bool {
    (6..=16).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_digit())
}

impl Content<'_> {
    pub fn subject(&self) -> &'static str {
        match self {
            Self::SigninCode { .. } => "Your Motion sign-in code",
            Self::RegistrationCode { .. } => "Your Motion verification code",
            Self::Invitation { .. } => "You are invited to Motion",
            Self::AccountNotice { .. } => "Motion account request",
            Self::Test { .. } => "Motion delivery test",
        }
    }
    pub fn text(&self) -> Result<String> {
        Ok(match self {
            Self::SigninCode { code } => {
                ensure!(code_ok(code), "invalid email sign-in code");
                format!(
                    "Your Motion sign-in code is {code}.\n\nEnter it only in the Motion browser that requested it. If you did\nnot request this code, ignore this email.\n"
                )
            }
            Self::RegistrationCode { code, origin } => {
                ensure!(code_ok(code), "invalid verification code");
                format!(
                    "Your Motion verification code is {code}.\n\nEnter it in the browser where you started creating an account at\n{origin}\n\nThe code expires in 10 minutes. If you did not request it, ignore this\nemail; no account is created without the code.\n"
                )
            }
            Self::Invitation { origin, expires_at } => format!(
                "An administrator of the Motion server at\n{origin}\ninvited this address to create an account.\n\nTo accept, open the page below, choose \"Create an account\" and enter\nthis email address. You will receive a separate verification code.\n{origin}/signin\n\nThe invitation expires {}.\nThis message contains no password or sign-in link.\n",
                message::rfc5322_date(*expires_at)
            ),
            Self::AccountNotice { origin } => format!(
                "Someone asked to create a Motion account with this address at\n{origin}\n\nThis address already has a sign-in method, so no account was created\nor changed. To sign in, choose \"Sign in by email\" at\n{origin}/signin\n\nIf this was not you, no action is needed.\n"
            ),
            Self::Test { code, origin } => {
                ensure!(code_ok(code), "invalid test code");
                format!(
                    "This is a delivery test requested by a Motion administrator at\n{origin}\n\nTest code: {code}\n\nEnter it in Authentication & Access to confirm receipt. This code\ncannot sign anyone in.\n"
                )
            }
        })
    }
}

pub struct Outgoing {
    pub bytes: Vec<u8>,
    pub message_id: String,
}

pub struct Signing<'a> {
    pub selector: &'a str,
    pub pkcs8: &'a [u8],
}

/// Compose, and in direct mode DKIM-sign, the exact bytes later transmitted
/// on every attempt. `id_domain` names the Message-ID right-hand side.
pub fn render(
    content: &Content<'_>,
    from: &Mailbox,
    to: &str,
    id: &str,
    id_domain: &str,
    date: i64,
    signing: Option<Signing<'_>>,
) -> Result<Outgoing> {
    let message_id = format!("<{id}@{id_domain}>");
    let text = content.text()?;
    let composed = message::compose(message::Compose {
        from,
        to,
        subject: content.subject(),
        text: &text,
        message_id: &message_id,
        date,
    })?
    .bytes();
    let bytes = match signing {
        None => composed,
        Some(signing) => dkim::sign(
            &composed,
            &dkim::Signer {
                domain: from.domain(),
                selector: signing.selector,
                pkcs8: signing.pkcs8,
                timestamp: date,
            },
        )
        .map_err(|_| anyhow!("message could not be signed"))?,
    };
    Ok(Outgoing { bytes, message_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_relay_is_only_available_to_a_literal_loopback_fixture() {
        let mut config = Relay {
            host: "127.0.0.1".into(),
            port: 2525,
            username: None,
            password: None,
            loopback_test: true,
        };
        assert!(validate_relay(&config).is_ok());
        for host in [
            "localhost",
            "127.0.0.1.evil.test",
            "192.168.1.1",
            "smtp.example.test",
        ] {
            config.host = host.into();
            assert!(validate_relay(&config).is_err());
        }
        config.loopback_test = false;
        assert!(validate_relay(&config).is_ok());
        config.username = Some("user".into());
        assert!(validate_relay(&config).is_err());
    }

    #[test]
    fn every_purpose_renders_bounded_ascii_text() {
        for content in [
            Content::SigninCode { code: "12345678" },
            Content::RegistrationCode {
                code: "12345678",
                origin: "https://motion.example.com",
            },
            Content::Invitation {
                origin: "https://motion.example.com",
                expires_at: 1_791_990_000,
            },
            Content::AccountNotice {
                origin: "https://motion.example.com",
            },
            Content::Test {
                code: "12345678",
                origin: "https://motion.example.com",
            },
        ] {
            let text = content.text().unwrap();
            assert!(
                text.is_ascii() && text.lines().all(|l| l.len() <= 78),
                "{text}"
            );
        }
        assert!(Content::SigninCode { code: "12ab" }.text().is_err());
    }
}
