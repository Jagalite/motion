//! OIDC adapters. These functions verify protocol observations and perform
//! bounded I/O; they never decide local membership, consume transactions or issue
//! Motion credentials. The caller must durably reserve an exchange attempt before
//! calling `exchange` and pass the resulting facts through the production core.
//!
//! Protocol errors deliberately discard upstream response bodies and URLs. They
//! may contain authorization codes, client secrets or provider tokens.

use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow, ensure};
use axum::http;
use openidconnect::{
    AccessTokenHash, AsyncHttpClient, AuthorizationCode, ClientId, ClientSecret, CsrfToken,
    EndpointMaybeSet, EndpointNotSet, EndpointSet, HttpRequest, HttpResponse, IssuerUrl, Nonce,
    OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
    core::{
        CoreAuthPrompt, CoreAuthenticationFlow, CoreClient, CoreJwsSigningAlgorithm,
        CoreProviderMetadata,
    },
};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 16 * 1024;
const MAX_JWKS_KEYS: usize = 32;
const MAX_DESTINATIONS: usize = 8;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const AUTH_MAX_AGE: i64 = 300;
const CLOCK_SKEW: i64 = 30;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub origin: String,
    /// An explicit connection pin. Required for private destinations and the
    /// isolated HTTP loopback fixture. DNS is not consulted for a pinned origin.
    pub address: Option<IpAddr>,
}

#[derive(Clone)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub destinations: Vec<Destination>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedIdentity {
    pub issuer: String,
    pub subject: String,
    /// Display metadata only; this is never an account-linking key.
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub authenticated_at: i64,
}

type DiscoveredClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

#[derive(Clone)]
struct DestinationPolicy {
    origins: BTreeMap<String, Option<IpAddr>>,
}

fn parse_url(value: &str) -> Result<Url> {
    ensure!(value.len() <= 2048, "sign-in URL is too long");
    let url = Url::parse(value).map_err(|_| anyhow!("invalid sign-in URL"))?;
    ensure!(
        matches!(url.scheme(), "https" | "http")
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.port_or_known_default().is_some_and(|port| port != 0),
        "invalid sign-in URL"
    );
    Ok(url)
}

fn literal_address(url: &Url) -> Option<IpAddr> {
    match url.host()? {
        Host::Ipv4(address) => Some(IpAddr::V4(address)),
        Host::Ipv6(address) => Some(IpAddr::V6(address)),
        Host::Domain(_) => None,
    }
}

fn usable_pin(address: IpAddr) -> bool {
    !address.is_unspecified()
        && !address.is_multicast()
        && match address {
            IpAddr::V4(address) => address != Ipv4Addr::BROADCAST,
            IpAddr::V6(address) => address.to_ipv4_mapped().is_none(),
        }
}

/// Conservative public-unicast policy. Explicit pins permit private IdPs without
/// making arbitrary local-network, metadata or transition addresses reachable.
pub(crate) fn is_public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let [a, b, c, _] = address.octets();
            !matches!(a, 0 | 10 | 127 | 224..=255)
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 169 && b == 254)
                && !(a == 172 && (16..=31).contains(&b))
                && !(a == 192
                    && ((b == 0 && matches!(c, 0 | 2)) || (b == 88 && c == 99) || b == 168))
                && !(a == 198 && (matches!(b, 18 | 19) || (b == 51 && c == 100)))
                && !(a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(address) => {
            let segments = address.segments();
            // Excludes mapped IPv4, NAT64, link-local, unique-local and multicast.
            (segments[0] & 0xe000) == 0x2000
                && !(segments[0] == 0x2001 && (segments[1] <= 0x01ff || segments[1] == 0x0db8))
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] <= 0x0fff)
        }
    }
}

impl DestinationPolicy {
    fn new(config: &OidcConfig) -> Result<Self> {
        ensure!(
            !config.client_id.trim().is_empty() && config.client_id.len() <= 512,
            "invalid OIDC client ID"
        );
        ensure!(
            config
                .client_secret
                .as_ref()
                .is_none_or(|secret| !secret.is_empty() && secret.len() <= 8192),
            "invalid OIDC client secret"
        );
        ensure!(
            config.destinations.len() <= MAX_DESTINATIONS,
            "too many OIDC destinations"
        );
        let issuer = parse_url(&config.issuer)?;
        ensure!(issuer.query().is_none(), "issuer URL cannot have a query");
        let mut origins = BTreeMap::new();
        for destination in &config.destinations {
            let url = parse_url(&destination.origin)?;
            ensure!(
                url.path() == "/" && url.query().is_none(),
                "destination must be an origin without a path or query"
            );
            if let Some(address) = destination.address {
                ensure!(usable_pin(address), "invalid OIDC destination address");
                ensure!(
                    literal_address(&url).is_none_or(|literal| literal == address),
                    "destination address disagrees with its literal host"
                );
            }
            ensure!(
                url.scheme() == "https"
                    || destination.address.is_some_and(|address| {
                        address.is_loopback() && literal_address(&url) == Some(address)
                    }),
                "HTTP requires an explicitly pinned loopback destination"
            );
            ensure!(
                origins
                    .insert(url.origin().ascii_serialization(), destination.address)
                    .is_none(),
                "duplicate OIDC destination"
            );
        }
        // A public HTTPS issuer needs no separate duplicate allowlist entry.
        // Private issuers still require an explicit address pin above.
        origins
            .entry(issuer.origin().ascii_serialization())
            .or_insert(None);
        let policy = Self { origins };
        policy.validate_url(&issuer)?;

        let callback = parse_url(&config.redirect_uri)?;
        ensure!(
            callback.query().is_none(),
            "callback URL cannot have a query"
        );
        if callback.scheme() != "https" {
            // The redirect is not requested by this adapter, but an HTTP
            // callback must be explicitly approved just like the test IdP.
            policy.validate_url(&callback)?;
        }
        Ok(policy)
    }

    fn validate_url(&self, url: &Url) -> Result<Option<IpAddr>> {
        // Callers may receive a URL parsed by another library, so apply the same
        // structural checks to every discovery/token/JWKS/authorization endpoint.
        parse_url(url.as_str())?;
        let pin = *self
            .origins
            .get(&url.origin().ascii_serialization())
            .ok_or_else(|| anyhow!("OIDC endpoint origin is not approved"))?;
        ensure!(
            url.scheme() == "https"
                || pin.is_some_and(|address| {
                    address.is_loopback() && literal_address(url) == Some(address)
                }),
            "HTTP requires an explicitly pinned loopback destination"
        );
        if let Some(literal) = literal_address(url) {
            ensure!(
                pin.map_or_else(|| is_public_address(literal), |pin| pin == literal),
                "OIDC endpoint address is not approved"
            );
        }
        Ok(pin)
    }

    async fn addresses(&self, url: &Url) -> Result<Vec<SocketAddr>, HttpFailure> {
        let pin = self
            .validate_url(url)
            .map_err(|_| HttpFailure("OIDC endpoint is not approved"))?;
        let port = url
            .port_or_known_default()
            .ok_or(HttpFailure("OIDC endpoint has no port"))?;
        if let Some(address) = pin.or_else(|| literal_address(url)) {
            return Ok(vec![SocketAddr::new(address, port)]);
        }
        let host = url
            .host_str()
            .ok_or(HttpFailure("OIDC endpoint has no host"))?;
        let addresses: Vec<_> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| HttpFailure("OIDC endpoint DNS lookup failed"))?
            .take(17)
            .collect();
        if addresses.is_empty()
            || addresses.len() > 16
            || addresses
                .iter()
                .any(|address| !is_public_address(address.ip()))
        {
            return Err(HttpFailure("OIDC endpoint DNS addresses are not approved"));
        }
        Ok(addresses)
    }
}

#[derive(Debug)]
struct HttpFailure(&'static str);

impl fmt::Display for HttpFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for HttpFailure {}

struct BoundedHttp {
    policy: DestinationPolicy,
}

impl BoundedHttp {
    async fn perform(&self, request: HttpRequest) -> Result<HttpResponse, HttpFailure> {
        if request.body().len() > MAX_REQUEST_BYTES
            || !matches!(*request.method(), http::Method::GET | http::Method::POST)
        {
            return Err(HttpFailure("invalid OIDC request"));
        }
        let url = parse_url(&request.uri().to_string())
            .map_err(|_| HttpFailure("invalid OIDC endpoint"))?;
        let addresses = self.policy.addresses(&url).await?;
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10));
        if let Some(Host::Domain(host)) = url.host() {
            // Pin the exact addresses that passed validation, avoiding a second
            // DNS query between the policy decision and the TCP connection.
            builder = builder.resolve_to_addrs(host, &addresses);
        }
        let client = builder
            .build()
            .map_err(|_| HttpFailure("OIDC HTTP client could not be configured"))?;
        let request =
            reqwest::Request::try_from(request).map_err(|_| HttpFailure("invalid OIDC request"))?;
        let mut response = client
            .execute(request)
            .await
            .map_err(|_| HttpFailure("OIDC endpoint request failed"))?;
        if response.status().is_redirection() {
            return Err(HttpFailure("OIDC endpoint redirect rejected"));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(HttpFailure("OIDC endpoint response is too large"));
        }
        let mut result = http::Response::builder().status(response.status());
        *result
            .headers_mut()
            .ok_or(HttpFailure("invalid OIDC response"))? = response.headers().clone();
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| HttpFailure("OIDC endpoint response failed"))?
        {
            if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
                return Err(HttpFailure("OIDC endpoint response is too large"));
            }
            body.extend_from_slice(&chunk);
        }
        result
            .body(body)
            .map_err(|_| HttpFailure("invalid OIDC response"))
    }
}

impl<'a> AsyncHttpClient<'a> for BoundedHttp {
    type Error = HttpFailure;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, HttpFailure>> + Send + 'a>>;

    fn call(&'a self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            tokio::time::timeout(REQUEST_TIMEOUT, self.perform(request))
                .await
                .map_err(|_| HttpFailure("OIDC endpoint request timed out"))?
        })
    }
}

async fn discover(config: &OidcConfig) -> Result<(DiscoveredClient, BoundedHttp)> {
    let http = BoundedHttp {
        policy: DestinationPolicy::new(config)?,
    };
    let metadata = CoreProviderMetadata::discover_async(
        IssuerUrl::new(config.issuer.clone()).map_err(|_| anyhow!("invalid OIDC issuer"))?,
        &http,
    )
    .await
    .map_err(|_| anyhow!("OIDC discovery or signing-key retrieval failed"))?;
    ensure!(
        !metadata.jwks().keys().is_empty() && metadata.jwks().keys().len() <= MAX_JWKS_KEYS,
        "OIDC signing-key count is outside the supported bounds"
    );
    http.policy
        .validate_url(metadata.authorization_endpoint().url())?;
    let token = metadata
        .token_endpoint()
        .ok_or_else(|| anyhow!("OIDC provider has no token endpoint"))?;
    http.policy.validate_url(token.url())?;
    ensure!(
        metadata.response_types_supported().iter().any(|types| {
            types.len() == 1 && types[0] == openidconnect::core::CoreResponseType::Code
        }),
        "OIDC provider does not advertise authorization code flow"
    );
    let client = CoreClient::from_provider_metadata(
        metadata,
        ClientId::new(config.client_id.clone()),
        config.client_secret.clone().map(ClientSecret::new),
    )
    .set_redirect_uri(
        RedirectUrl::new(config.redirect_uri.clone())
            .map_err(|_| anyhow!("invalid OIDC callback URL"))?,
    );
    Ok((client, http))
}

/// Connectivity/configuration check only. Successful browser proof is recorded
/// independently by the caller; this function cannot enable a provider.
pub async fn check_oidc(config: &OidcConfig) -> Result<()> {
    discover(config).await.map(|_| ())
}

pub async fn authorization_url(
    config: &OidcConfig,
    state: &str,
    nonce: &str,
    pkce_verifier: &str,
) -> Result<String> {
    validate_transaction_secrets(state, nonce, pkce_verifier)?;
    let (client, _) = discover(config).await?;
    let challenge = PkceCodeChallenge::from_code_verifier_sha256(&PkceCodeVerifier::new(
        pkce_verifier.to_owned(),
    ));
    let state = CsrfToken::new(state.to_owned());
    let nonce = Nonce::new(nonce.to_owned());
    let (url, _, _) = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            move || state,
            move || nonce,
        )
        .add_scope(Scope::new("email".to_owned()))
        .add_scope(Scope::new("profile".to_owned()))
        .set_pkce_challenge(challenge)
        .set_max_age(Duration::from_secs(AUTH_MAX_AGE as u64))
        .add_prompt(CoreAuthPrompt::Login)
        .url();
    Ok(url.to_string())
}

fn validate_transaction_secrets(state: &str, nonce: &str, verifier: &str) -> Result<()> {
    ensure!(
        (32..=512).contains(&state.len()) && (32..=512).contains(&nonce.len()),
        "invalid OIDC transaction binding"
    );
    ensure!(
        (43..=128).contains(&verifier.len())
            && verifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte)),
        "invalid OIDC PKCE verifier"
    );
    Ok(())
}

fn validate_claim_times(
    issued_at: i64,
    authenticated_at: i64,
    expires_at: i64,
    now: i64,
) -> Result<()> {
    ensure!(
        issued_at > 0
            && issued_at <= now.saturating_add(CLOCK_SKEW)
            && issued_at >= now.saturating_sub(AUTH_MAX_AGE + CLOCK_SKEW)
            && expires_at > now
            && expires_at > issued_at,
        "OIDC token lifetime is not valid"
    );
    ensure!(
        authenticated_at > 0
            && authenticated_at <= now.saturating_add(CLOCK_SKEW)
            && authenticated_at <= issued_at.saturating_add(CLOCK_SKEW)
            && authenticated_at >= now.saturating_sub(AUTH_MAX_AGE),
        "OIDC authentication is not recent"
    );
    Ok(())
}

fn validate_authorized_party(client_id: &str, audiences: usize, party: Option<&str>) -> Result<()> {
    ensure!(
        party.is_none_or(|party| party == client_id) && (audiences <= 1 || party.is_some()),
        "OIDC authorized party does not match this client"
    );
    Ok(())
}

/// One code exchange, with no retries and no cached keys. A crash or uncertain
/// outcome requires a new transaction. Fresh discovery means each admitted
/// transaction sees current signing keys without an unbounded unknown-kid loop.
pub async fn exchange(
    config: &OidcConfig,
    code: &str,
    verifier: &str,
    nonce: &str,
) -> Result<VerifiedIdentity> {
    validate_transaction_secrets(nonce, nonce, verifier)?;
    ensure!(!code.is_empty() && code.len() <= 8192, "invalid OIDC code");
    let (client, http) = discover(config).await?;
    let response = client
        .exchange_code(AuthorizationCode::new(code.to_owned()))
        .map_err(|_| anyhow!("OIDC token endpoint is unavailable"))?
        .set_pkce_verifier(PkceCodeVerifier::new(verifier.to_owned()))
        .request_async(&http)
        .await
        .map_err(|_| anyhow!("OIDC code exchange failed"))?;
    let token = response
        .id_token()
        .ok_or_else(|| anyhow!("OIDC provider did not return an ID token"))?;
    let token_verifier = client.id_token_verifier().set_allowed_algs([
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
        CoreJwsSigningAlgorithm::EcdsaP256Sha256,
        CoreJwsSigningAlgorithm::EdDsa,
    ]);
    let claims = token
        .claims(&token_verifier, &Nonce::new(nonce.to_owned()))
        .map_err(|_| anyhow!("OIDC ID token verification failed"))?;
    validate_authorized_party(
        &config.client_id,
        claims.audiences().len(),
        claims.authorized_party().map(|party| party.as_str()),
    )?;
    let authenticated_at = claims
        .auth_time()
        .ok_or_else(|| anyhow!("OIDC ID token has no authentication time"))?
        .timestamp();
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| anyhow!("server clock is invalid"))?
            .as_secs(),
    )
    .map_err(|_| anyhow!("server clock is invalid"))?;
    validate_claim_times(
        claims.issue_time().timestamp(),
        authenticated_at,
        claims.expiration().timestamp(),
        now,
    )?;
    if let Some(expected_hash) = claims.access_token_hash() {
        let actual_hash = AccessTokenHash::from_token(
            response.access_token(),
            token
                .signing_alg()
                .map_err(|_| anyhow!("invalid OIDC signing algorithm"))?,
            token
                .signing_key(&token_verifier)
                .map_err(|_| anyhow!("invalid OIDC signing key"))?,
        )
        .map_err(|_| anyhow!("OIDC access token binding failed"))?;
        ensure!(
            *expected_hash == actual_hash,
            "OIDC access token binding failed"
        );
    }
    let subject = claims.subject().as_str();
    ensure!(
        !subject.is_empty() && subject.len() <= 255,
        "OIDC subject is invalid"
    );
    Ok(VerifiedIdentity {
        issuer: claims.issuer().as_str().to_owned(),
        subject: subject.to_owned(),
        email: claims
            .email()
            .filter(|email| email.as_str().len() <= 320)
            .map(|email| email.as_str().to_owned()),
        display_name: claims
            .name()
            .and_then(|name| name.get(None))
            .filter(|name| name.as_str().len() <= 256)
            .map(|name| name.as_str().to_owned()),
        authenticated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn local_http_response(response: Vec<u8>) -> (BoundedHttp, HttpRequest) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let _ = stream.read(&mut request).await;
            let _ = stream.write_all(&response).await;
            let _ = stream.shutdown().await;
        });
        let origin = format!("http://{address}");
        let mut config = config();
        config.issuer = origin.clone();
        config.destinations.push(Destination {
            origin: origin.clone(),
            address: Some(address.ip()),
        });
        (
            BoundedHttp {
                policy: DestinationPolicy::new(&config).unwrap(),
            },
            http::Request::builder()
                .uri(format!("{origin}/keys"))
                .body(Vec::new())
                .unwrap(),
        )
    }

    fn config() -> OidcConfig {
        OidcConfig {
            issuer: "https://identity.example.test/tenant".into(),
            client_id: "motion-test".into(),
            client_secret: None,
            redirect_uri: "https://motion.example.test/callback".into(),
            destinations: vec![],
        }
    }

    #[test]
    fn endpoint_policy_rejects_origin_confusion_and_non_https_urls() {
        let policy = DestinationPolicy::new(&config()).unwrap();
        assert!(
            policy
                .validate_url(&Url::parse("https://identity.example.test/keys").unwrap())
                .is_ok()
        );
        for url in [
            "http://identity.example.test/keys",
            "https://identity.example.test:8443/keys",
            "https://identity.example.test.evil.test/keys",
            "https://identity.example.test@evil.test/keys",
            "https://user@identity.example.test/keys",
            "https://identity.example.test/keys#fragment",
            "https://127.0.0.1/keys",
        ] {
            assert!(
                policy.validate_url(&Url::parse(url).unwrap()).is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn private_and_loopback_exceptions_bind_exact_origin_and_address() {
        let mut config = config();
        config.issuer = "http://127.0.0.1:45678".into();
        assert!(DestinationPolicy::new(&config).is_err());
        config.destinations.push(Destination {
            origin: config.issuer.clone(),
            address: Some("127.0.0.1".parse().unwrap()),
        });
        let policy = DestinationPolicy::new(&config).unwrap();
        assert!(
            policy
                .validate_url(&Url::parse("http://127.0.0.1:45678/keys").unwrap())
                .is_ok()
        );
        assert!(
            policy
                .validate_url(&Url::parse("http://127.0.0.1:45679/keys").unwrap())
                .is_err()
        );
        config.destinations[0].address = Some("127.0.0.2".parse().unwrap());
        assert!(DestinationPolicy::new(&config).is_err());
        config.destinations[0].address = Some("10.0.0.1".parse().unwrap());
        assert!(DestinationPolicy::new(&config).is_err());
        config.issuer = "https://10.0.0.1".into();
        config.destinations[0].origin = config.issuer.clone();
        assert!(DestinationPolicy::new(&config).is_ok());
        config.issuer = "http://public.example.test".into();
        config.destinations[0].origin = config.issuer.clone();
        config.destinations[0].address = Some("127.0.0.1".parse().unwrap());
        assert!(DestinationPolicy::new(&config).is_err());
    }

    #[test]
    fn callback_http_requires_its_own_exact_loopback_approval() {
        let mut config = config();
        config.redirect_uri = "http://127.0.0.1:3333/callback".into();
        assert!(DestinationPolicy::new(&config).is_err());
        config.destinations.push(Destination {
            origin: "http://127.0.0.1:3333".into(),
            address: Some("127.0.0.1".parse().unwrap()),
        });
        assert!(DestinationPolicy::new(&config).is_ok());
        config.redirect_uri = "http://127.0.0.1:3334/callback".into();
        assert!(DestinationPolicy::new(&config).is_err());
    }

    #[test]
    fn public_dns_policy_rejects_special_and_transition_addresses() {
        for address in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.168.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "64:ff9b::a00:1",
            "2002:a00:1::",
            "2001::1",
            "2001:db8::1",
            "3fff::1",
        ] {
            assert!(!is_public_address(address.parse().unwrap()), "{address}");
        }
        for address in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(is_public_address(address.parse().unwrap()), "{address}");
        }
    }

    #[test]
    fn token_policy_requires_recent_proof_and_correct_authorized_party() {
        assert!(validate_claim_times(1000, 1000, 1300, 1000).is_ok());
        assert!(validate_claim_times(1000, 700, 1300, 1000).is_ok());
        for (issued, authenticated, expires) in [
            (1031, 1000, 1300),
            (669, 669, 1300),
            (1000, 699, 1300),
            (1000, 1031, 1300),
            (1000, 1000, 1000),
            (1001, 1000, 1001),
        ] {
            assert!(validate_claim_times(issued, authenticated, expires, 1000).is_err());
        }
        assert!(validate_authorized_party("motion", 1, None).is_ok());
        assert!(validate_authorized_party("motion", 1, Some("motion")).is_ok());
        assert!(validate_authorized_party("motion", 1, Some("other")).is_err());
        assert!(validate_authorized_party("motion", 2, None).is_err());
    }

    #[test]
    fn pkce_policy_checks_rfc7636_length_and_alphabet() {
        let binding = "a".repeat(32);
        assert!(validate_transaction_secrets(&binding, &binding, &"v".repeat(43)).is_ok());
        assert!(validate_transaction_secrets(&binding, &binding, &"v".repeat(42)).is_err());
        assert!(validate_transaction_secrets(&binding, &binding, &"v".repeat(129)).is_err());
        assert!(validate_transaction_secrets(&binding, &binding, &"+".repeat(43)).is_err());
    }

    #[tokio::test]
    async fn http_adapter_preserves_success_without_following_redirects() {
        let (client, request) = local_http_response(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec(),
        ).await;
        let response = client.call(request).await.unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.body(), b"{}");
        assert_eq!(
            response.headers()[http::header::CONTENT_TYPE],
            "application/json"
        );

        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{}/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            target.local_addr().unwrap(),
        );
        let (client, request) = local_http_response(redirect.into_bytes()).await;
        assert_eq!(
            client.call(request).await.unwrap_err().0,
            "OIDC endpoint redirect rejected"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), target.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn http_adapter_caps_content_length_and_unadvertised_streams() {
        let advertised = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_RESPONSE_BYTES + 1,
        );
        let (client, request) = local_http_response(advertised.into_bytes()).await;
        assert_eq!(
            client.call(request).await.unwrap_err().0,
            "OIDC endpoint response is too large"
        );

        let mut streamed = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        streamed.extend(vec![b'x'; MAX_RESPONSE_BYTES + 1]);
        let (client, request) = local_http_response(streamed).await;
        assert_eq!(
            client.call(request).await.unwrap_err().0,
            "OIDC endpoint response is too large"
        );
    }
}
