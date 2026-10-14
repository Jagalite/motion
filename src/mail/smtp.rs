//! Minimal SMTP client for native direct delivery (RFC 5321 + STARTTLS,
//! RFC 3207). One recipient, one 7-bit message, no AUTH, no pipelining.
//!
//! Replies are bounded in line length, line count and total bytes; every read
//! and write has a deadline. STARTTLS discards all pre-TLS state, rejects
//! plaintext bytes buffered after the `220` reply (command injection) and
//! repeats EHLO. Each step maps to a `mail::Observation` so the production
//! core, not this adapter, decides retry, failover and terminal state.
use std::{net::SocketAddr, sync::Arc, time::Duration};

use playscale_core::mail::{Observation, Stage, classify_reply};
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
        BufReader,
    },
    net::TcpStream,
    time::{Instant, timeout_at},
};
use tokio_rustls::{
    TlsConnector,
    rustls::{self, pki_types::ServerName},
};

pub const MAX_REPLY_LINE: usize = 2048;
pub const MAX_REPLY_LINES: usize = 64;
pub const MAX_REPLY_BYTES: usize = 32 * 1024;
// Worst case per route: 8 + 8 × 20 + 40 + 40 = 248 s. The attempt starts no
// new route after `direct::ROUTE_CUTOFF`, keeping it inside `mail::LEASE_SECONDS`.
pub const CONNECT: Duration = Duration::from_secs(8);
pub const COMMAND: Duration = Duration::from_secs(20);
pub const FINAL_REPLY: Duration = Duration::from_secs(40);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub code: u16,
    pub lines: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    Io,
    Timeout,
    Malformed,
    TooLarge,
}

/// Read one possibly multiline reply. All lines must carry the same code and
/// continuation lines use `-`; the last uses a space or ends after the code.
pub async fn read_reply<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    deadline: Instant,
) -> Result<Reply, ReadError> {
    let mut lines = Vec::new();
    let mut total = 0usize;
    let mut code = None;
    loop {
        let mut line = Vec::new();
        let read = timeout_at(
            deadline,
            (&mut *reader)
                .take(MAX_REPLY_LINE as u64 + 2)
                .read_until(b'\n', &mut line),
        )
        .await
        .map_err(|_| ReadError::Timeout)?
        .map_err(|_| ReadError::Io)?;
        if read == 0 {
            return Err(ReadError::Io);
        }
        if !line.ends_with(b"\n") {
            return Err(ReadError::TooLarge);
        }
        total += line.len();
        if total > MAX_REPLY_BYTES || lines.len() >= MAX_REPLY_LINES {
            return Err(ReadError::TooLarge);
        }
        let line = line
            .strip_suffix(b"\r\n")
            .ok_or(ReadError::Malformed)?
            .to_vec();
        let text = String::from_utf8(line).map_err(|_| ReadError::Malformed)?;
        let bytes = text.as_bytes();
        if bytes.len() < 3
            || !bytes[..3].iter().all(u8::is_ascii_digit)
            || !(b'2'..=b'5').contains(&bytes[0])
        {
            return Err(ReadError::Malformed);
        }
        let this: u16 = text[..3].parse().map_err(|_| ReadError::Malformed)?;
        if code.is_some_and(|code| code != this) {
            return Err(ReadError::Malformed);
        }
        code = Some(this);
        let (more, rest) = match bytes.get(3) {
            None => (false, ""),
            Some(b' ') => (false, &text[4..]),
            Some(b'-') => (true, &text[4..]),
            Some(_) => return Err(ReadError::Malformed),
        };
        if rest.bytes().any(|b| b.is_ascii_control() && b != b'\t') {
            return Err(ReadError::Malformed);
        }
        lines.push(rest.to_owned());
        if !more {
            return Ok(Reply { code: this, lines });
        }
    }
}

/// Dot-stuff CRLF message bytes and append the terminator (RFC 5321 §4.5.2).
pub fn dot_stuff(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 16);
    let mut start = true;
    for &b in message {
        if start && b == b'.' {
            out.push(b'.');
        }
        out.push(b);
        start = b == b'\n';
    }
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}

#[derive(Clone)]
pub enum Tls {
    /// STARTTLS is mandatory and the certificate must validate for the host.
    Required(Arc<rustls::RootCertStore>),
    /// Explicit isolated loopback qualification route only.
    LoopbackPlaintext,
}

pub struct Session<'a> {
    pub route_host: &'a str,
    pub address: SocketAddr,
    pub helo: &'a str,
    pub mail_from: &'a str,
    pub rcpt_to: &'a str,
    pub tls: &'a Tls,
    pub message: &'a [u8],
}

/// What happened on one route, with a redacted diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteOutcome {
    pub observation: Observation,
    pub detail: String,
}

fn outcome(observation: Observation, detail: impl Into<String>) -> RouteOutcome {
    RouteOutcome {
        observation,
        detail: detail.into(),
    }
}

struct Conn<S> {
    io: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Conn<S> {
    async fn command(&mut self, line: &str, stage: Stage) -> Result<Reply, RouteOutcome> {
        let deadline = Instant::now() + COMMAND;
        timeout_at(deadline, async {
            self.io.get_mut().write_all(line.as_bytes()).await?;
            self.io.get_mut().write_all(b"\r\n").await?;
            self.io.get_mut().flush().await
        })
        .await
        .map_err(|_| {
            outcome(
                Observation::Temporary(stage),
                format!("{} timeout", stage.as_str()),
            )
        })?
        .map_err(|_| {
            outcome(
                Observation::Temporary(stage),
                format!("{} connection lost", stage.as_str()),
            )
        })?;
        self.reply(stage, deadline).await
    }
    async fn reply(&mut self, stage: Stage, deadline: Instant) -> Result<Reply, RouteOutcome> {
        read_reply(&mut self.io, deadline).await.map_err(|error| {
            outcome(
                Observation::Temporary(stage),
                format!("{} {error:?}", stage.as_str()).to_ascii_lowercase(),
            )
        })
    }
    async fn expect(
        &mut self,
        line: &str,
        stage: Stage,
        ok: impl Fn(u16) -> bool,
    ) -> Result<Reply, RouteOutcome> {
        let reply = self.command(line, stage).await?;
        if ok(reply.code) {
            Ok(reply)
        } else {
            Err(outcome(
                classify_reply(stage, reply.code),
                format!("{} {}", stage.as_str(), reply.code),
            ))
        }
    }
    async fn quit(&mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            let _ = self.io.get_mut().write_all(b"QUIT\r\n").await;
            let _ = self.io.get_mut().flush().await;
        })
        .await;
    }
}

/// A handshake error from certificate validation or protocol incompatibility,
/// as opposed to a transport failure that may succeed later.
fn policy_failure(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .is_some_and(|error| {
            matches!(
                error,
                rustls::Error::InvalidCertificate(_)
                    | rustls::Error::NoCertificatesPresented
                    | rustls::Error::PeerIncompatible(_)
                    | rustls::Error::AlertReceived(_)
            )
        })
}

fn starttls_offered(reply: &Reply) -> bool {
    reply
        .lines
        .iter()
        .skip(1)
        .any(|line| line.trim().eq_ignore_ascii_case("STARTTLS"))
}

/// Run the envelope and message on an established, possibly TLS, connection.
/// `recheck` runs after RCPT and before DATA; secret content is sent only if it
/// returns true.
async fn transact<S, F>(conn: &mut Conn<S>, session: &Session<'_>, recheck: F) -> RouteOutcome
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: std::future::Future<Output = bool>,
{
    if let Err(failure) = conn
        .expect(
            &format!("MAIL FROM:<{}>", session.mail_from),
            Stage::MailFrom,
            |c| c == 250,
        )
        .await
    {
        return failure;
    }
    if let Err(failure) = conn
        .expect(
            &format!("RCPT TO:<{}>", session.rcpt_to),
            Stage::RcptTo,
            |c| c == 250 || c == 251,
        )
        .await
    {
        return failure;
    }
    if !recheck.await {
        conn.quit().await;
        return outcome(Observation::Withdrawn, "withdrawn before data");
    }
    if let Err(failure) = conn.expect("DATA", Stage::Data, |c| c == 354).await {
        return failure;
    }
    let payload = dot_stuff(session.message);
    let written = timeout_at(Instant::now() + FINAL_REPLY, async {
        conn.io.get_mut().write_all(&payload).await?;
        conn.io.get_mut().flush().await
    })
    .await;
    // The terminator is the final bytes written. If writing failed, the server
    // cannot have received a complete message.
    if !matches!(written, Ok(Ok(()))) {
        return outcome(
            Observation::Temporary(Stage::Message),
            "message write failed",
        );
    }
    match read_reply(&mut conn.io, Instant::now() + FINAL_REPLY).await {
        Ok(reply) if (200..300).contains(&reply.code) => {
            conn.quit().await;
            outcome(Observation::Accepted, format!("accepted {}", reply.code))
        }
        Ok(reply) => {
            conn.quit().await;
            outcome(
                classify_reply(Stage::Message, reply.code),
                format!("message {}", reply.code),
            )
        }
        Err(error) => outcome(
            Observation::Ambiguous,
            format!("final reply {error:?}").to_ascii_lowercase(),
        ),
    }
}

/// Deliver one message over one route.
pub async fn deliver<F>(session: &Session<'_>, recheck: F) -> RouteOutcome
where
    F: std::future::Future<Output = bool>,
{
    let stream = match tokio::time::timeout(CONNECT, TcpStream::connect(session.address)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) => return outcome(Observation::Temporary(Stage::Connect), "connect failed"),
        Err(_) => return outcome(Observation::Temporary(Stage::Connect), "connect timeout"),
    };
    let mut conn = Conn {
        io: BufReader::with_capacity(4096, stream),
    };
    match conn.reply(Stage::Greeting, Instant::now() + COMMAND).await {
        // RFC 5321 §3.1: a 554 greeting means no service here; try the next
        // exchanger rather than treating it as a refusal of this message.
        Ok(reply) if reply.code == 220 => {}
        Ok(reply) => {
            conn.quit().await;
            return outcome(
                Observation::Temporary(Stage::Greeting),
                format!("greeting {}", reply.code),
            );
        }
        Err(failure) => return failure,
    }
    let ehlo = format!("EHLO {}", session.helo);
    let capabilities = match conn.expect(&ehlo, Stage::Ehlo, |c| c == 250).await {
        Ok(reply) => reply,
        Err(failure) => {
            conn.quit().await;
            return outcome(Observation::Temporary(Stage::Ehlo), failure.detail);
        }
    };
    let roots = match session.tls {
        Tls::LoopbackPlaintext => return transact(&mut conn, session, recheck).await,
        Tls::Required(roots) => roots.clone(),
    };
    if !starttls_offered(&capabilities) {
        conn.quit().await;
        return outcome(
            Observation::Temporary(Stage::TlsPolicy),
            "starttls not offered",
        );
    }
    match conn.command("STARTTLS", Stage::Tls).await {
        Ok(reply) if reply.code == 220 => {}
        Ok(reply) => {
            conn.quit().await;
            // 454 is transient; a 5xx refusal of STARTTLS is a definite policy failure.
            let stage = if reply.code >= 500 {
                Stage::TlsPolicy
            } else {
                Stage::Tls
            };
            return outcome(
                Observation::Temporary(stage),
                format!("starttls {}", reply.code),
            );
        }
        Err(failure) => return outcome(Observation::Temporary(Stage::Tls), failure.detail),
    }
    if !conn.io.buffer().is_empty() {
        return outcome(
            Observation::Temporary(Stage::TlsPolicy),
            "plaintext data after starttls",
        );
    }
    let Ok(name) = ServerName::try_from(session.route_host.to_owned()) else {
        return outcome(Observation::Temporary(Stage::TlsPolicy), "invalid tls name");
    };
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map(|builder| builder.with_root_certificates(roots).with_no_client_auth());
    let Ok(config) = config else {
        return outcome(Observation::Temporary(Stage::Tls), "tls configuration");
    };
    let tcp = conn.io.into_inner();
    let tls = match tokio::time::timeout(
        COMMAND,
        TlsConnector::from(Arc::new(config)).connect(name, tcp),
    )
    .await
    {
        Ok(Ok(tls)) => tls,
        Ok(Err(error)) if policy_failure(&error) => {
            return outcome(
                Observation::Temporary(Stage::TlsPolicy),
                "tls certificate or protocol rejected",
            );
        }
        Ok(Err(_)) => return outcome(Observation::Temporary(Stage::Tls), "tls handshake failed"),
        Err(_) => return outcome(Observation::Temporary(Stage::Tls), "tls handshake timeout"),
    };
    let mut conn = Conn {
        io: BufReader::with_capacity(4096, tls),
    };
    if let Err(failure) = conn.expect(&ehlo, Stage::Ehlo, |c| c == 250).await {
        conn.quit().await;
        return outcome(Observation::Temporary(Stage::Ehlo), failure.detail);
    }
    transact(&mut conn, session, recheck).await
}

/// Connectivity probe without any mail transaction: greeting, EHLO, STARTTLS
/// and certificate validation, then QUIT. Returns the observed local address,
/// the furthest stage reached and a redacted detail.
pub struct Probe {
    pub local: Option<SocketAddr>,
    pub reached: &'static str,
    pub ok: bool,
    pub detail: String,
}

pub async fn probe(route_host: &str, address: SocketAddr, helo: &str, tls: &Tls) -> Probe {
    let fail = |local, reached, detail: &str| Probe {
        local,
        reached,
        ok: false,
        detail: detail.to_owned(),
    };
    let stream = match tokio::time::timeout(CONNECT, TcpStream::connect(address)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) => return fail(None, "connect", "outbound connection failed"),
        Err(_) => return fail(None, "connect", "outbound connection timed out"),
    };
    let local = stream.local_addr().ok();
    let mut conn = Conn {
        io: BufReader::with_capacity(4096, stream),
    };
    match conn.reply(Stage::Greeting, Instant::now() + COMMAND).await {
        Ok(reply) if reply.code == 220 => {}
        Ok(_) | Err(_) => return fail(local, "greeting", "no usable SMTP greeting"),
    }
    let ehlo = format!("EHLO {helo}");
    let Ok(capabilities) = conn.expect(&ehlo, Stage::Ehlo, |c| c == 250).await else {
        return fail(local, "ehlo", "EHLO was refused");
    };
    let roots = match tls {
        Tls::LoopbackPlaintext => {
            conn.quit().await;
            return Probe {
                local,
                reached: "ehlo",
                ok: true,
                detail: "loopback qualification route; TLS not used".into(),
            };
        }
        Tls::Required(roots) => roots.clone(),
    };
    if !starttls_offered(&capabilities) {
        conn.quit().await;
        return fail(local, "tls", "STARTTLS is not offered");
    }
    match conn.command("STARTTLS", Stage::Tls).await {
        Ok(reply) if reply.code == 220 && conn.io.buffer().is_empty() => {}
        _ => return fail(local, "tls", "STARTTLS was refused"),
    }
    let Ok(name) = ServerName::try_from(route_host.to_owned()) else {
        return fail(local, "tls", "invalid TLS name");
    };
    let Ok(builder) = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions() else {
        return fail(local, "tls", "TLS configuration");
    };
    let config = builder.with_root_certificates(roots).with_no_client_auth();
    let tcp = conn.io.into_inner();
    match tokio::time::timeout(
        COMMAND,
        TlsConnector::from(Arc::new(config)).connect(name, tcp),
    )
    .await
    {
        Ok(Ok(tls)) => {
            let mut conn = Conn {
                io: BufReader::with_capacity(4096, tls),
            };
            conn.quit().await;
            Probe {
                local,
                reached: "tls",
                ok: true,
                detail: "STARTTLS negotiated with a certificate valid for the exchanger".into(),
            }
        }
        _ => fail(
            local,
            "tls",
            "TLS handshake or certificate validation failed",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn parse(input: &[u8]) -> Result<Reply, ReadError> {
        let mut reader = BufReader::new(input);
        read_reply(&mut reader, Instant::now() + Duration::from_secs(1)).await
    }

    #[tokio::test]
    async fn replies_are_bounded_and_consistent() {
        assert_eq!(
            parse(b"250-mx.example\r\n250-STARTTLS\r\n250 SIZE 1000\r\n").await,
            Ok(Reply {
                code: 250,
                lines: vec!["mx.example".into(), "STARTTLS".into(), "SIZE 1000".into()]
            })
        );
        assert_eq!(parse(b"220\r\n").await.unwrap().code, 220);
        for bad in [
            &b"250-a\r\n251 b\r\n"[..],
            b"25\r\n",
            b"650 x\r\n",
            b"250x\r\n",
            b"250 a\n",
            b"250 a\x00b\r\n",
            b"250-a\r\n",
            b"",
        ] {
            assert!(parse(bad).await.is_err(), "{bad:?}");
        }
        let long = format!("250 {}\r\n", "a".repeat(MAX_REPLY_LINE));
        assert_eq!(parse(long.as_bytes()).await, Err(ReadError::TooLarge));
        let many = "250-a\r\n".repeat(MAX_REPLY_LINES) + "250 a\r\n";
        assert_eq!(parse(many.as_bytes()).await, Err(ReadError::TooLarge));
    }

    #[test]
    fn dot_stuffing_escapes_leading_dots_and_terminates() {
        assert_eq!(dot_stuff(b"a\r\n.b\r\n..\r\n"), b"a\r\n..b\r\n...\r\n.\r\n");
        assert_eq!(dot_stuff(b".x"), b"..x\r\n.\r\n");
    }
}
