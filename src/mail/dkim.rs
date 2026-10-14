//! DKIM (RFC 6376) rsa-sha256 signing with relaxed/relaxed canonicalization.
//!
//! `ring` performs RSA signing (constant-time, PKCS#1 v1.5). The `rsa` crate is
//! used only to generate keys and decode imported PKCS#1/PKCS#8 PEM; it never
//! performs private-key operations on message data.
use anyhow::{Result, anyhow, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{rand::SystemRandom, signature};
use rsa::{
    RsaPrivateKey, RsaPublicKey,
    pkcs1::DecodeRsaPrivateKey,
    pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey},
    traits::PublicKeyParts,
};
use sha2::{Digest, Sha256};

/// Headers signed in this order. `from` is listed twice (over-signing) so a
/// second From header added in transit invalidates the signature.
pub const SIGNED_HEADERS: &[&str] = &[
    "from",
    "from",
    "to",
    "subject",
    "date",
    "message-id",
    "mime-version",
    "content-type",
    "content-transfer-encoding",
    "auto-submitted",
];

pub struct KeyMaterial {
    /// PKCS#8 DER private key, to be sealed before storage.
    pub pkcs8: Vec<u8>,
    /// SubjectPublicKeyInfo DER, base64, for the DNS `p=` tag.
    pub public: String,
}

fn material(key: &RsaPrivateKey) -> Result<KeyMaterial> {
    let bits = key.size() * 8;
    ensure!(
        (2048..=4096).contains(&bits),
        "DKIM RSA keys must be 2048–4096 bits"
    );
    let pkcs8 = key
        .to_pkcs8_der()
        .map_err(|_| anyhow!("DKIM key could not be encoded"))?
        .as_bytes()
        .to_vec();
    // Reject anything ring would not sign with, before it is ever stored.
    signature::RsaKeyPair::from_pkcs8(&pkcs8).map_err(|_| anyhow!("unsupported DKIM key"))?;
    let public = RsaPublicKey::from(key)
        .to_public_key_der()
        .map_err(|_| anyhow!("DKIM key could not be encoded"))?;
    Ok(KeyMaterial {
        pkcs8,
        public: STANDARD.encode(public.as_bytes()),
    })
}

/// Generate a new 2048-bit key. CPU-bound; run on a blocking thread.
pub fn generate() -> Result<KeyMaterial> {
    let key = RsaPrivateKey::new(&mut rand_core::OsRng, 2048)
        .map_err(|_| anyhow!("DKIM key generation failed"))?;
    material(&key)
}

/// Import a PEM private key (PKCS#8 `PRIVATE KEY` or PKCS#1 `RSA PRIVATE KEY`).
pub fn import(pem: &str) -> Result<KeyMaterial> {
    ensure!(pem.len() <= 16 * 1024, "DKIM key is too large");
    let pem = pem.trim();
    let key = RsaPrivateKey::from_pkcs8_pem(pem)
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(pem))
        .map_err(|_| anyhow!("Enter an unencrypted RSA private key in PEM form"))?;
    material(&key)
}

/// Stable signing-key identity: the first 16 hex digits of SHA-256(SPKI).
pub fn key_id(public: &str) -> String {
    let digest = Sha256::digest(public.as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// The TXT record value to publish at `<selector>._domainkey.<domain>`.
pub fn dns_record(public: &str) -> String {
    format!("v=DKIM1; k=rsa; p={public}")
}

pub fn selector_ok(selector: &str) -> bool {
    (1..=63).contains(&selector.len())
        && selector.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn is_wsp(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// Relaxed header canonicalization (RFC 6376 §3.4.2) of `name: value`.
pub fn relaxed_header(name: &str, value: &str) -> String {
    let unfolded = value.replace("\r\n", "");
    let mut out = String::with_capacity(unfolded.len());
    let mut space = false;
    for b in unfolded.bytes() {
        if is_wsp(b) {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(b as char);
        }
    }
    format!("{}:{}", name.trim().to_ascii_lowercase(), out)
}

/// Relaxed body canonicalization (RFC 6376 §3.4.4) of CRLF text.
pub fn relaxed_body(body: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(body);
    let mut lines: Vec<String> = text
        .split("\r\n")
        .map(|line| {
            let mut out = String::with_capacity(line.len());
            let mut space = false;
            for b in line.bytes() {
                if is_wsp(b) {
                    space = true;
                } else {
                    if space {
                        out.push(' ');
                    }
                    space = false;
                    out.push(b as char);
                }
            }
            out
        })
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let mut out = Vec::with_capacity(body.len());
    for line in lines {
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out
}

pub struct Signer<'a> {
    pub domain: &'a str,
    pub selector: &'a str,
    pub pkcs8: &'a [u8],
    pub timestamp: i64,
}

/// Sign `message` (headers, blank line, body; CRLF) and return the bytes with a
/// `DKIM-Signature` header prepended. Only headers in `SIGNED_HEADERS` that are
/// present exactly once are accepted, so the signed set is unambiguous.
pub fn sign(message: &[u8], signer: &Signer<'_>) -> Result<Vec<u8>> {
    ensure!(
        selector_ok(signer.selector) && signer.domain.is_ascii() && !signer.domain.is_empty(),
        "invalid DKIM selector or domain"
    );
    let text = std::str::from_utf8(message).map_err(|_| anyhow!("message is not ASCII"))?;
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow!("message has no body separator"))?;
    let mut headers: Vec<(&str, &str)> = Vec::new();
    for line in head.split("\r\n") {
        ensure!(
            !line.starts_with([' ', '\t']),
            "folded headers are not produced by Motion"
        );
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow!("malformed header"))?;
        headers.push((name, value));
    }
    let mut canonical = String::new();
    let mut seen = std::collections::BTreeSet::new();
    for name in SIGNED_HEADERS {
        if !seen.insert(*name) {
            // Over-signed: the absent second instance contributes nothing.
            continue;
        }
        let found: Vec<_> = headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .collect();
        ensure!(found.len() == 1, "signed header {name} must appear once");
        canonical.push_str(&relaxed_header(found[0].0, found[0].1));
        canonical.push_str("\r\n");
    }
    let body_hash = STANDARD.encode(Sha256::digest(relaxed_body(body.as_bytes())));
    let value = format!(
        " v=1; a=rsa-sha256; c=relaxed/relaxed; d={}; s={};\r\n\tt={}; h={};\r\n\tbh={};\r\n\tb=",
        signer.domain,
        signer.selector,
        signer.timestamp,
        SIGNED_HEADERS.join(":"),
        body_hash
    );
    canonical.push_str(&relaxed_header("DKIM-Signature", &value));
    let pair = signature::RsaKeyPair::from_pkcs8(signer.pkcs8)
        .map_err(|_| anyhow!("unsupported DKIM key"))?;
    let mut sig = vec![0; pair.public().modulus_len()];
    pair.sign(
        &signature::RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        canonical.as_bytes(),
        &mut sig,
    )
    .map_err(|_| anyhow!("DKIM signing failed"))?;
    let encoded = STANDARD.encode(sig);
    let folded: Vec<&str> = encoded
        .as_bytes()
        .chunks(72)
        .map(|chunk| std::str::from_utf8(chunk).unwrap())
        .collect();
    let mut out = format!("DKIM-Signature:{value}{}\r\n", folded.join("\r\n\t")).into_bytes();
    out.extend_from_slice(message);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaxed_canonicalization_matches_rfc6376_examples() {
        // RFC 6376 §3.4.5 example.
        assert_eq!(relaxed_header("A", " X"), "a:X");
        assert_eq!(relaxed_header("B ", " Y\t\r\n\tZ  "), "b:Y Z");
        assert_eq!(
            relaxed_body(b" C \r\nD \t E\r\n\r\n\r\n"),
            b" C\r\nD E\r\n".to_vec()
        );
        assert_eq!(relaxed_body(b""), Vec::<u8>::new());
    }

    #[test]
    fn selectors_are_dns_labels() {
        assert!(selector_ok("motion2026"));
        assert!(selector_ok("a.b-c"));
        for bad in ["", "-a", "a-", "a..b", "a_b", "a b", &"a".repeat(64)] {
            assert!(!selector_ok(bad), "{bad:?}");
        }
    }
}
