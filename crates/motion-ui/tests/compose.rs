//! Composition proof for the Topcoat presentation inside Axum (plan 11.7, 12.5,
//! 14.4, 14.5). Uses the self-identifying mock facade; it proves routing,
//! authorization plumbing, headers and markup, not backend behaviour.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::Request,
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use http_body_util::BodyExt;
use motion_ui::{
    CONTENT_SECURITY_POLICY, UiPrincipal, assets, facade::ProfileOption, mock::MockUiQueryFacade,
};
use tower::ServiceExt;

fn principal(profile: &str, permissions: &[&str]) -> UiPrincipal {
    UiPrincipal {
        principal_id: "p1".into(),
        profile_id: profile.into(),
        profile_name: profile.into(),
        profiles: vec![ProfileOption {
            id: profile.into(),
            name: profile.into(),
        }],
        permissions: permissions.iter().map(|p| p.to_string()).collect(),
        server_epoch: "epoch1".into(),
        csrf_token: "csrf-abc".into(),
    }
}

const VIEWER: &[&str] = &["catalog:read", "playback:request", "viewing:write"];

/// Stand-in for the host's authentication layer: it alone inserts the principal.
fn app(who: Option<UiPrincipal>, demuxe: Option<std::path::PathBuf>) -> Router {
    let api = Router::new().route(
        "/api/v2/system/health",
        get(|| async {
            (
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"status":"ok"}"#,
            )
        }),
    );
    motion_ui::mount(api, Arc::new(MockUiQueryFacade), demuxe).layer(middleware::from_fn(
        move |mut request: Request, next: Next| {
            let who = who.clone();
            async move {
                if let Some(who) = who {
                    request.extensions_mut().insert(who);
                }
                next.run(request).await
            }
        },
    ))
}

async fn get_page(app: &Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    send(app, Method::GET, uri).await
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let response: Response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

/// No inline script, no event-handler attributes, no runtime/eval machinery.
fn assert_strict_markup(html: &str) {
    let lower = html.to_lowercase();
    for (index, _) in lower.match_indices("<script") {
        let tag_end = lower[index..].find('>').unwrap() + index;
        let tag = &lower[index..tag_end];
        assert!(tag.contains(" src="), "inline script: {tag}");
        assert!(
            lower[tag_end + 1..].starts_with("</script>"),
            "script element has inline content"
        );
    }
    for needle in [
        " onclick=",
        " onload=",
        " onerror=",
        " oninput=",
        "javascript:",
        "new function",
        "eval(",
    ] {
        assert!(!lower.contains(needle), "markup contains {needle}");
    }
}

fn assert_presentation_headers(headers: &axum::http::HeaderMap) {
    assert_eq!(headers["content-security-policy"], CONTENT_SECURITY_POLICY);
    assert!(!CONTENT_SECURITY_POLICY.contains("'unsafe-eval'"));
    assert!(!CONTENT_SECURITY_POLICY.contains("'unsafe-inline'"));
    assert!(!CONTENT_SECURITY_POLICY.contains("'unsafe-hashes'"));
    // The only inline allowance is the pinned Demuxe stylesheet hash.
    assert!(CONTENT_SECURITY_POLICY.contains(&format!(
        "style-src 'self' '{}'",
        motion_ui::DEMUXE_STYLE_HASH
    )));
    assert_eq!(headers["cache-control"], "private, no-store");
    assert_eq!(headers["cross-origin-opener-policy"], "same-origin");
    assert_eq!(headers["cross-origin-embedder-policy"], "require-corp");
    assert_eq!(headers["x-content-type-options"], "nosniff");
}

#[tokio::test]
async fn unauthenticated_pages_render_sign_in_with_401() {
    let app = app(None, None);
    let (status, headers, html) = get_page(&app, "/").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_presentation_headers(&headers);
    assert!(html.contains("Sign in to Motion"));
    assert!(
        !html.contains("Films"),
        "no library data without a principal"
    );
    assert_strict_markup(&html);
    let (status, _, html) = get_page(&app, "/item/item1").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!html.contains("Arrival of the Night Train"));
}

#[tokio::test]
async fn home_renders_for_a_principal_and_labels_the_mock() {
    let app = app(Some(principal("everyone", VIEWER)), None);
    let (status, headers, html) = get_page(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert_presentation_headers(&headers);
    assert!(html.contains("Development mock — not a Motion server"));
    assert!(html.contains("Films") && html.contains("Partly available"));
    assert!(html.contains("Resume at 30:30"));
    assert!(html.contains(r#"href="/library/lib1""#));
    assert!(html.contains(r#"<meta name="motion-csrf" content="csrf-abc">"#));
    assert!(html.contains(&format!(r#"src="{}""#, assets::BRIDGE.url())));
    assert_strict_markup(&html);
}

#[tokio::test]
async fn api_paths_never_fall_through_to_html() {
    let app = app(Some(principal("everyone", VIEWER)), None);
    for uri in [
        "/api/v2/unknown",
        "/api/v2",
        "/api/v2/",
        "/api/v2/catalog/items/x/nope",
    ] {
        let (status, headers, body) = get_page(&app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(
            headers[header::CONTENT_TYPE],
            "application/problem+json",
            "{uri}"
        );
        assert!(body.contains(r#""code":"not_found""#));
        assert!(
            !headers.contains_key("content-security-policy"),
            "presentation headers leaked onto API"
        );
    }
    let (status, headers, body) = get_page(&app, "/api/v2/system/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, r#"{"status":"ok"}"#);
    assert!(!headers.contains_key("content-security-policy"));
}

#[tokio::test]
async fn restricted_titles_are_indistinguishable_from_missing() {
    let kids = app(Some(principal("kids", VIEWER)), None);
    let (restricted, _, restricted_html) = get_page(&kids, "/item/item3").await;
    let (missing, _, missing_html) = get_page(&kids, "/item/no-such-item").await;
    assert_eq!(restricted, StatusCode::NOT_FOUND);
    assert_eq!(missing, StatusCode::NOT_FOUND);
    assert_eq!(restricted_html, missing_html);
    assert!(!restricted_html.contains("Midnight Ledger"));
    let (_, _, library) = get_page(&kids, "/library/lib1").await;
    assert!(!library.contains("Midnight Ledger"));
    let (_, _, search) = get_page(&kids, "/search?q=midnight").await;
    assert!(!search.contains("Midnight Ledger") && search.contains("No titles match."));
    let (_, _, play) = get_page(&kids, "/play/tl3").await;
    assert!(!play.contains("Midnight Ledger"));

    let adult = app(Some(principal("everyone", VIEWER)), None);
    let (status, _, html) = get_page(&adult, "/item/item3").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Midnight Ledger"));
}

#[tokio::test]
async fn permissions_gate_pages_and_actions() {
    let browse_only = app(Some(principal("everyone", &["catalog:read"])), None);
    let (status, _, html) = get_page(&browse_only, "/item/item1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !html.contains("/play/tl1"),
        "no play action without playback:request"
    );
    let (status, _, _) = get_page(&browse_only, "/play/tl1").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let nothing = app(Some(principal("everyone", &[])), None);
    let (status, _, html) = get_page(&nothing, "/").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(html.contains("You don’t have access"));
}

#[tokio::test]
async fn series_navigation_and_generated_links() {
    let app = app(Some(principal("everyone", VIEWER)), None);
    let (_, _, series) = get_page(&app, "/item/item4").await;
    assert!(series.contains(r#"href="/item/item5""#));
    let (_, _, season) = get_page(&app, "/item/item5").await;
    assert!(season.contains("Pilot") && season.contains("The Second Lamp"));
    let (_, _, episode) = get_page(&app, "/item/item6").await;
    assert!(episode.contains(r#"href="/play/tl6""#) && episode.contains("Resume"));
}

#[tokio::test]
async fn user_text_is_escaped_and_bounded() {
    let app = app(Some(principal("everyone", VIEWER)), None);
    let (status, _, html) = get_page(&app, "/search?q=%3Cscript%3Ealert(1)%3C%2Fscript%3E").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!html.contains("<script>alert(1)"));
    assert!(html.contains("&lt;script&gt;"));
    assert_strict_markup(&html);
    let long = "a".repeat(201);
    let (status, _, _) = get_page(&app, &format!("/search?q={long}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn player_page_emits_a_stable_host_and_external_module() {
    let app = app(Some(principal("everyone", VIEWER)), None);
    let (status, _, html) = get_page(&app, "/play/tl1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(r#"id="motion-player""#));
    assert!(html.contains(r#"data-timeline-id="tl1""#));
    assert!(html.contains(r#"data-principal-id="p1""#));
    assert!(html.contains(r#"data-server-epoch="epoch1""#));
    assert!(html.contains(r#"data-can-save-viewing="true""#));
    assert!(html.contains(r#"id="motion-playback-controls""#));
    assert!(html.contains(r#"name="version""#));
    assert!(html.contains(r#"data-resume-ms="1830000""#));
    assert!(html.contains(r#"data-demuxe-base="/assets/demuxe/""#));
    assert!(html.contains(&format!(r#"src="{}""#, assets::PLAYER.url())));
    assert_strict_markup(&html);
}

#[tokio::test]
async fn ui_assets_are_content_hashed_and_immutable() {
    let app = app(None, None);
    for asset in assets::all() {
        let (status, headers, body) = get_page(&app, &asset.url()).await;
        assert_eq!(status, StatusCode::OK, "{}", asset.url());
        assert_eq!(headers[header::CONTENT_TYPE], asset.content_type);
        assert_eq!(
            headers[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(body.as_bytes(), asset.bytes);
    }
    let (status, _, _) = get_page(&app, "/ui/bridge.000000000000.js").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stale hash must not resolve");
    let (status, _, _) = get_page(&app, "/ui/..%2fCargo.toml").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn demuxe_is_served_as_its_own_unrenamed_tree() {
    let dir = std::env::temp_dir().join(format!("motion-ui-demuxe-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("web/generated/player")).unwrap();
    std::fs::write(
        dir.join("web/generated/player/index.js"),
        "export const x = 1;",
    )
    .unwrap();
    std::fs::write(dir.join("core.wasm"), [0, 97, 115, 109]).unwrap();
    let app = app(None, Some(dir.clone()));
    let (status, headers, body) =
        get_page(&app, "/assets/demuxe/web/generated/player/index.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("javascript")
    );
    assert_eq!(body, "export const x = 1;");
    let (status, headers, _) = get_page(&app, "/assets/demuxe/core.wasm").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/wasm");
    let (status, _, _) = get_page(&app, "/assets/demuxe/../Cargo.toml").await;
    assert_ne!(status, StatusCode::OK);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn head_requests_and_unknown_pages() {
    let app = app(Some(principal("everyone", VIEWER)), None);
    let (status, _, body) = send(&app, Method::HEAD, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    let (status, headers, _) = get_page(&app, "/no/such/page").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_presentation_headers(&headers);
}

const ADMIN: &[&str] = &[
    "catalog:read",
    "catalog:write",
    "sources:manage",
    "profiles:manage",
    "viewing:write",
    "playback:request",
    "processing:request",
    "system:admin",
];

#[tokio::test]
async fn administrative_screens_require_their_permissions() {
    let viewer = app(Some(principal("everyone", VIEWER)), None);
    for uri in ["/sources", "/matches", "/processing", "/diagnostics"] {
        let (status, headers, html) = get_page(&viewer, uri).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        assert_presentation_headers(&headers);
        assert!(!html.contains("data-command=\"POST /api/v2/sources\""));
    }
    let (_, _, home) = get_page(&viewer, "/").await;
    assert!(!home.contains(r#"href="/sources""#) && !home.contains(r#"href="/diagnostics""#));
    assert!(home.contains(r#"href="/profiles""#));

    let admin = app(Some(principal("everyone", ADMIN)), None);
    let (_, _, home) = get_page(&admin, "/").await;
    for link in [
        "/matches",
        "/processing",
        "/sources",
        "/profiles",
        "/diagnostics",
    ] {
        assert!(home.contains(&format!(r#"href="{link}""#)), "{link}");
    }
    for uri in [
        "/sources",
        "/matches",
        "/processing",
        "/diagnostics",
        "/profiles",
    ] {
        let (status, _, html) = get_page(&admin, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_strict_markup(&html);
    }
}

#[tokio::test]
async fn command_forms_name_public_operations_with_preconditions() {
    let admin = app(Some(principal("everyone", ADMIN)), None);
    let (_, _, sources) = get_page(&admin, "/sources").await;
    assert!(sources.contains(r#"data-command="POST /api/v2/sources" data-idempotent="true""#));
    assert!(
        sources
            .contains(r#"data-command="POST /api/v2/libraries/lib1/scans" data-idempotent="true""#)
    );
    assert!(sources.contains(r#"data-command="POST /api/v2/libraries" data-idempotent="true""#));
    assert!(
        sources.contains("Partial — some folders could not be read; nothing unseen was removed")
    );
    assert!(sources.contains("1 incomplete"));
    let (_, _, matches) = get_page(&admin, "/matches").await;
    assert!(matches.contains(r#"data-command="PUT /api/v2/catalog/matches/m1/decision" data-idempotent="false" data-if-match="&quot;r1&quot;""#));
    assert!(matches.contains(r#"value="reject""#) && matches.contains(r#"value="defer""#));
    let (_, _, processing) = get_page(&admin, "/processing").await;
    assert!(processing.contains(r#"data-command="POST /api/v2/jobs/job2/cancel""#));
    assert!(
        !processing.contains("/api/v2/jobs/job1/cancel"),
        "finished jobs offer no cancel"
    );
    let (_, _, profiles) = get_page(&admin, "/profiles").await;
    assert!(profiles.contains(r#"data-command="PUT /api/v2/profiles/everyone/preferences" data-idempotent="false" data-if-match="&quot;prefs-everyone-1&quot;""#));
    assert!(profiles.contains(r#"<option value="foreign_audio" selected="">"#));
}

#[tokio::test]
async fn sign_in_offers_pairing_and_profiles_switch_among_allowed_profiles() {
    let anonymous = app(None, None);
    let (status, _, html) = get_page(&anonymous, "/profiles").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(html.contains(r#"data-command="POST /api/v2/auth/pairings""#));
    assert!(
        !html.contains("device_code"),
        "device codes never appear in HTML"
    );

    let mut who = principal("kids", VIEWER);
    who.profiles = vec![
        ProfileOption {
            id: "everyone".into(),
            name: "Everyone".into(),
        },
        ProfileOption {
            id: "kids".into(),
            name: "Kids".into(),
        },
    ];
    let app = app(Some(who), None);
    let (_, _, html) = get_page(&app, "/").await;
    assert!(html.contains(r#"data-profile-switch="motion_profile""#));
    assert!(html.contains(r#"<option value="kids" selected="">Kids</option>"#));
    assert!(html.contains(r#"<option value="everyone">Everyone</option>"#));
}

#[tokio::test]
async fn command_bodies_satisfy_required_contract_fields() {
    let admin = app(Some(principal("everyone", ADMIN)), None);
    let (_, _, sources) = get_page(&admin, "/sources").await;
    // LibraryInput.source_ids is required even when no source is checked.
    assert!(
        sources.contains(r#"<input type="hidden" name="source_ids" data-type="json" value="[]">"#)
    );
    let (_, _, profiles) = get_page(&admin, "/profiles").await;
    // Preferences.completion_percent is required (50..=100); the input cannot be left empty.
    assert!(profiles.contains(r#"name="completion_percent" data-type="number" required="""#));
    let (_, _, matches) = get_page(&admin, "/matches").await;
    // A match without candidates cannot be accepted, but can be rejected or deferred.
    let m2 = &matches
        [matches.find(r#"id="match-m2""#).unwrap()..matches.find(r#"id="match-m1""#).unwrap()];
    assert!(m2.contains("No candidates were proposed."));
    assert!(!m2.contains(r#"value="accept""#));
    assert!(m2.contains(r#"value="reject""#) && m2.contains(r#"value="defer""#));
}

#[tokio::test]
async fn playback_without_viewing_permission_does_not_offer_writing_authority() {
    let app = app(
        Some(principal("everyone", &["catalog:read", "playback:request"])),
        None,
    );
    let (status, _, html) = get_page(&app, "/play/tl1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(r#"data-can-save-viewing="false""#));
    assert!(!html.contains(r#"data-can-save-viewing="true""#));
}
