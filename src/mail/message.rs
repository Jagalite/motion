//! Narrow RFC 5322 message construction for Motion's own transactional mail.
//!
//! Supported forms are deliberately small: ASCII dot-atom addresses (the shared
//! `canonical_email` policy), an optional printable-ASCII display name, fixed
//! ASCII subjects and 7-bit plain-text bodies with CRLF line endings. Anything
//! else is rejected instead of encoded; no caller-supplied text can introduce
//! a header line.
use anyhow::{Result, ensure};
use playscale_core::signin::canonical_email;

/// RFC 5322 §2.1.1 limit, excluding CRLF.
const MAX_LINE: usize = 998;
const MAX_BODY: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mailbox {
    pub name: Option<String>,
    pub address: String,
}

impl Mailbox {
    pub fn domain(&self) -> &str {
        self.address.rsplit_once('@').map(|(_, d)| d).unwrap_or("")
    }
    fn render(&self) -> String {
        match &self.name {
            Some(name) => format!("\"{name}\" <{}>", self.address),
            None => format!("<{}>", self.address),
        }
    }
}

fn display_name_ok(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\')
        && name.trim() == name
}

/// Parse `Name <address>` or `address`. The address must already satisfy the
/// shared canonical policy; it is stored lower-cased.
pub fn parse_mailbox(value: &str) -> Option<Mailbox> {
    let value = value.trim();
    if value.len() > 320 || value.bytes().any(|b| b.is_ascii_control()) {
        return None;
    }
    let (name, address) = match value.strip_suffix('>') {
        Some(rest) => {
            let (name, address) = rest.rsplit_once('<')?;
            let name = name.trim().trim_matches('"').trim();
            (
                (!name.is_empty()).then(|| name.to_owned()),
                address.to_owned(),
            )
        }
        None => (None, value.to_owned()),
    };
    if name.as_deref().is_some_and(|name| !display_name_ok(name)) {
        return None;
    }
    let address = canonical_email(&address).filter(|a| a == &address.to_ascii_lowercase())?;
    Some(Mailbox { name, address })
}

/// A fully rendered message: header block and body joined by an empty line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Message {
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        for (name, value) in &self.headers {
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        out.push_str(&self.body);
        out.into_bytes()
    }
}

/// RFC 5322 date-time in UTC, e.g. `Wed, 14 Oct 2026 08:00:00 +0000`.
pub fn rfc5322_date(unix: i64) -> String {
    let at =
        time::OffsetDateTime::from_unix_timestamp(unix).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let weekday = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
        [at.weekday().number_days_from_monday() as usize];
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][u8::from(at.month()) as usize - 1];
    format!(
        "{weekday}, {:02} {month} {:04} {:02}:{:02}:{:02} +0000",
        at.day(),
        at.year(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

fn header_value_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_LINE - 80
        && value
            .bytes()
            .all(|b| (0x20..0x7f).contains(&b) || b == b'\t')
}

/// Normalize body text to CRLF and verify it is 7-bit with bounded lines.
fn body(text: &str) -> Result<String> {
    ensure!(
        text.len() <= MAX_BODY && text.is_ascii(),
        "unsupported message body"
    );
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.trim_end_matches('\n').split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        ensure!(
            line.len() <= MAX_LINE
                && !line.contains('\r')
                && line
                    .bytes()
                    .all(|b| (0x20..0x7f).contains(&b) || b == b'\t'),
            "unsupported message body"
        );
        out.push_str(line);
        out.push_str("\r\n");
    }
    Ok(out)
}

pub struct Compose<'a> {
    pub from: &'a Mailbox,
    pub to: &'a str,
    pub subject: &'a str,
    pub text: &'a str,
    pub message_id: &'a str,
    pub date: i64,
}

pub fn compose(input: Compose<'_>) -> Result<Message> {
    let to = canonical_email(input.to)
        .filter(|a| a == input.to)
        .ok_or_else(|| anyhow::anyhow!("invalid email destination"))?;
    ensure!(
        canonical_email(&input.from.address).as_deref() == Some(input.from.address.as_str())
            && input.from.name.as_deref().is_none_or(display_name_ok),
        "invalid sender"
    );
    ensure!(
        header_value_ok(input.subject) && input.subject.len() <= 200,
        "invalid subject"
    );
    ensure!(
        input.message_id.starts_with('<')
            && input.message_id.ends_with('>')
            && input.message_id.len() <= 300
            && input.message_id[1..input.message_id.len() - 1]
                .split_once('@')
                .is_some_and(|(left, right)| {
                    !left.is_empty()
                        && !right.is_empty()
                        && (left.bytes().chain(right.bytes()))
                            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
                }),
        "invalid message identifier"
    );
    let headers = vec![
        ("Date".into(), rfc5322_date(input.date)),
        ("From".into(), input.from.render()),
        ("To".into(), format!("<{to}>")),
        ("Subject".into(), input.subject.to_owned()),
        ("Message-ID".into(), input.message_id.to_owned()),
        ("MIME-Version".into(), "1.0".into()),
        ("Content-Type".into(), "text/plain; charset=us-ascii".into()),
        ("Content-Transfer-Encoding".into(), "7bit".into()),
        ("Auto-Submitted".into(), "auto-generated".into()),
    ];
    for (_, value) in &headers {
        ensure!(header_value_ok(value), "invalid header");
    }
    Ok(Message {
        headers,
        body: body(input.text)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from() -> Mailbox {
        parse_mailbox("Motion <motion@example.com>").unwrap()
    }
    fn input<'a>(from: &'a Mailbox, to: &'a str, subject: &'a str, text: &'a str) -> Compose<'a> {
        Compose {
            from,
            to,
            subject,
            text,
            message_id: "<id-1@example.com>",
            date: 1_791_990_000,
        }
    }

    #[test]
    fn mailbox_parsing_accepts_only_supported_ascii_forms() {
        assert_eq!(
            parse_mailbox("\"Motion Server\" <motion@example.com>"),
            Some(Mailbox {
                name: Some("Motion Server".into()),
                address: "motion@example.com".into()
            })
        );
        assert_eq!(parse_mailbox("motion@example.com").unwrap().name, None);
        assert_eq!(
            parse_mailbox("Motion <MOTION@Example.com>")
                .unwrap()
                .address,
            "motion@example.com"
        );
        for bad in [
            "Mötion <motion@example.com>",
            "Motion\r\nBcc: x@example.com <motion@example.com>",
            "Mo\"tion <motion@example.com>",
            "Motion <motion@example.com>\r\nBcc: a@example.com",
            "motion@例.example",
            "Motion <motion@localhost>",
            "<>",
        ] {
            assert_eq!(parse_mailbox(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn headers_and_bodies_reject_injection_and_unsupported_encodings() {
        let from = from();
        assert!(compose(input(&from, "a@example.com", "Hi", "Body")).is_ok());
        for to in [
            "A@example.com",
            "a@example.com\r\nBcc: b@example.com",
            "a@example.com>, <b@example.com",
            "á@example.com",
        ] {
            assert!(compose(input(&from, to, "Hi", "Body")).is_err(), "{to:?}");
        }
        for subject in ["Hi\r\nBcc: b@example.com", "Hi\nX: y", "Grüße", ""] {
            assert!(
                compose(input(&from, "a@example.com", subject, "Body")).is_err(),
                "{subject:?}"
            );
        }
        for text in ["caf\u{e9}", "bare\rCR", &"x".repeat(999), "nul\0byte"] {
            assert!(
                compose(input(&from, "a@example.com", "Hi", text)).is_err(),
                "{text:?}"
            );
        }
        let mut bad_id = input(&from, "a@example.com", "Hi", "Body");
        bad_id.message_id = "<a@b>\r\nBcc: c@d";
        assert!(compose(bad_id).is_err());
    }

    #[test]
    fn rendered_message_uses_crlf_and_rfc5322_date() {
        let from = from();
        let message = compose(input(&from, "a@example.com", "Hi", "one\ntwo\r\n.three\n")).unwrap();
        let bytes = String::from_utf8(message.bytes()).unwrap();
        assert!(bytes.starts_with("Date: Wed, 14 Oct 2026"));
        assert!(bytes.contains("\r\nFrom: \"Motion\" <motion@example.com>\r\n"));
        assert!(bytes.ends_with("\r\n\r\none\r\ntwo\r\n.three\r\n"));
        assert!(!bytes.replace("\r\n", "").contains(['\r', '\n']));
        assert_eq!(rfc5322_date(0), "Thu, 01 Jan 1970 00:00:00 +0000");
    }
}
