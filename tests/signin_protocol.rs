//! Real HTTP/JWKS/JWT exchanges against an isolated adversarial provider.
use playscale::signin_protocol::{Destination, OidcConfig, authorization_url, exchange};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test]
async fn verified_exchange_rejects_wrong_claims_and_tracks_key_rotation() {
    let mut child = tokio::process::Command::new("node")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/qualification/auth-signin/protocol-provider.mjs"
        ))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("Node is required for the isolated protocol fixture");
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut port = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        output.read_line(&mut port),
    )
    .await
    .unwrap()
    .unwrap();
    let origin = format!("http://127.0.0.1:{}", port.trim().parse::<u16>().unwrap());
    let config = OidcConfig {
        issuer: origin.clone(),
        client_id: "motion-test".into(),
        client_secret: Some("fixture-only".into()),
        redirect_uri: format!("{origin}/callback"),
        destinations: vec![Destination {
            origin,
            address: Some("127.0.0.1".parse().unwrap()),
        }],
    };
    let nonce = "n".repeat(64);
    let verifier = "v".repeat(64);
    let url = authorization_url(&config, &"s".repeat(64), &nonce, &verifier)
        .await
        .unwrap();
    let parsed = url::Url::parse(&url).unwrap();
    let params: std::collections::BTreeMap<_, _> = parsed.query_pairs().collect();
    assert_eq!(
        params.get("code_challenge_method").map(|v| v.as_ref()),
        Some("S256")
    );
    assert_eq!(
        params.get("state").map(|v| v.as_ref()),
        Some("s".repeat(64).as_str())
    );
    let verified = exchange(&config, "valid", &verifier, &nonce).await.unwrap();
    assert_eq!(verified.subject, "verified-person");
    assert_eq!(verified.issuer, config.issuer);
    for code in [
        "issuer",
        "audience",
        "nonce",
        "expired",
        "azp",
        "old-auth",
        "signature",
        "unknown-key",
    ] {
        assert!(
            exchange(&config, code, &verifier, &nonce).await.is_err(),
            "accepted invalid {code}"
        );
    }
    assert!(
        exchange(&config, "valid", &"x".repeat(64), &nonce)
            .await
            .is_err(),
        "accepted wrong PKCE verifier"
    );
    // Rotation between discovery and exchange fails closed. A fresh transaction
    // fetches the current JWKS and succeeds without retrying the consumed code.
    assert!(
        exchange(&config, "rotate", &verifier, &nonce)
            .await
            .is_err()
    );
    assert_eq!(
        exchange(&config, "valid", &verifier, &nonce)
            .await
            .unwrap()
            .subject,
        "verified-person"
    );
    child.kill().await.unwrap();
}
