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
        viewing_epoch: "server:restore".into(),
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
    let (status, headers, body) = send_bytes(app, method, uri).await;
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

async fn send_bytes(
    app: &Router,
    method: Method,
    uri: &str,
) -> (StatusCode, axum::http::HeaderMap, axum::body::Bytes) {
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
    (status, headers, body)
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
    // Markers: a skip button the player reveals inside its interval, marked
    // uncertain when detected, and chapters that seek.
    assert!(
        html.contains(
            r#"data-player-seek="65000" data-marker-start="5000" data-marker-end="65000""#
        ),
        "{html}"
    );
    assert!(html.contains("Skip intro") && html.contains("(detected)"));
    assert!(html.contains(r#"data-player-seek="0""#) && html.contains("Departure"));
    assert_strict_markup(&html);
}

#[tokio::test]
async fn ui_assets_are_content_hashed_and_immutable() {
    let app = app(None, None);
    for asset in assets::all() {
        // Raw bytes: some assets (the icons) are binary.
        let (status, headers, body) = send_bytes(&app, Method::GET, &asset.url()).await;
        assert_eq!(status, StatusCode::OK, "{}", asset.url());
        assert_eq!(headers[header::CONTENT_TYPE], asset.content_type);
        assert_eq!(
            headers[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(body.as_ref(), asset.bytes);
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
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert!(!headers.contains_key(header::LAST_MODIFIED));
    assert!(!headers.contains_key(header::ETAG));
    let file = dir.join("web/generated/player/index.js");
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::fs::write(&file, "export const x = 2;").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        modified
    );
    let cached = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/assets/demuxe/web/generated/player/index.js")
                .header(header::IF_MODIFIED_SINCE, "Thu, 01 Jan 2099 00:00:00 GMT")
                .header(header::IF_NONE_MATCH, "*")
                .header(header::IF_RANGE, "Thu, 01 Jan 2099 00:00:00 GMT")
                .header(header::RANGE, "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cached.status(), StatusCode::OK);
    assert_eq!(cached.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        cached.into_body().collect().await.unwrap().to_bytes(),
        "export const x = 2;"
    );
    let (status, headers, _) = get_page(&app, "/assets/demuxe/core.wasm").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/wasm");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
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
    "collections:write",
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
    // Deletion explains its exact effect, needs explicit confirmation and is
    // guarded by the revision the page showed. A source a library still uses
    // is not offered for removal.
    assert!(sources.contains(r#"data-command="DELETE /api/v2/libraries/lib1" data-idempotent="false" data-if-match="&quot;r-4&quot;""#), "{sources}");
    assert!(sources.contains("nothing is deleted from disk"));
    assert!(
        sources.contains(r#"<input type="checkbox" required="">"#),
        "{sources}"
    );
    assert!(!sources.contains(r#"data-command="DELETE /api/v2/sources/src1""#));
    assert!(sources.contains("Remove it from those libraries first"));
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

#[tokio::test]
async fn phone_shell_installs_and_navigates_by_tabs() {
    let viewer = app(Some(principal("everyone", VIEWER)), None);
    let (status, _, home) = get_page(&viewer, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(home.contains("viewport-fit=cover"));
    assert!(home.contains(&format!(
        r#"<link rel="manifest" href="{}">"#,
        assets::MANIFEST_URL
    )));
    assert!(home.contains(&format!(r#"href="{}""#, assets::TOUCH_ICON.url())));
    let tabbar = &home[home.find(r#"<nav class="tabbar""#).expect("tab bar")..];
    let tabbar = &tabbar[..tabbar.find("</nav>").unwrap()];
    for link in ["/", "/libraries", "/search", "/profiles"] {
        assert!(tabbar.contains(&format!(r#"href="{link}""#)), "{link}");
    }
    assert!(
        !tabbar.contains(r#"href="/manage""#),
        "viewer has nothing to manage"
    );
    assert!(tabbar.contains(r#"class="tab-link is-current" href="/" aria-current="page""#));
    assert_strict_markup(&home);

    let (status, _, libraries) = get_page(&viewer, "/libraries").await;
    assert_eq!(status, StatusCode::OK);
    assert!(libraries.contains(r#"href="/library/lib1""#) && libraries.contains("Films"));
    assert!(libraries.contains(r#"class="tab-link is-current" href="/libraries""#));
    let (status, _, _) = get_page(&viewer, "/manage").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let admin = app(Some(principal("everyone", ADMIN)), None);
    let (status, _, manage) = get_page(&admin, "/manage").await;
    assert_eq!(status, StatusCode::OK);
    for link in ["/matches", "/processing", "/sources", "/diagnostics"] {
        assert!(
            manage.contains(&format!(r#"<a class="list-row" href="{link}""#)),
            "{link}"
        );
    }
    assert!(manage.contains(r#"class="tab-link is-current" href="/manage""#));
    let (_, _, sources) = get_page(&admin, "/sources").await;
    assert!(sources.contains(r#"class="tab-link is-current" href="/manage""#));

    // Phone CSS hides the app frame only around a rendered player
    // (.shell:has(.player-page)), so a playback error keeps its navigation.
    let (_, _, player) = get_page(&viewer, "/play/tl1").await;
    assert!(player.contains(r#"class="player-page""#));
    let (status, _, missing) = get_page(&viewer, "/play/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        !missing.contains(r#"class="player-page""#) && missing.contains(r#"<nav class="tabbar""#)
    );
}

#[tokio::test]
async fn web_manifest_names_this_builds_icons() {
    let app = app(None, None);
    let (status, headers, body) = get_page(&app, assets::MANIFEST_URL).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/manifest+json");
    assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
    let manifest: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(manifest["display"], "standalone");
    assert_eq!(manifest["start_url"], "/");
    for (icon, asset) in manifest["icons"]
        .as_array()
        .unwrap()
        .iter()
        .zip([&assets::ICON_192, &assets::ICON_512])
    {
        assert_eq!(icon["src"], asset.url());
        let (status, _, _) = get_page(&app, icon["src"].as_str().unwrap()).await;
        assert_eq!(status, StatusCode::OK);
    }
}

#[tokio::test]
async fn library_administration_offers_edit_relocation_and_schedules() {
    let admin = app(Some(principal("everyone", ADMIN)), None);
    let (_, _, sources) = get_page(&admin, "/sources").await;
    assert_strict_markup(&sources);
    // Library edits are checked against the library revision that was shown.
    assert!(sources.contains(r#"data-command="PUT /api/v2/libraries/lib1" data-idempotent="false" data-if-match="&quot;r-4&quot;""#), "{sources}");
    assert!(sources.contains(
        r#"<input type="checkbox" name="source_ids" value="src1" data-type="member" checked="">"#
    ));
    // Relocation previews against the source revision and names its commit.
    assert!(sources.contains(r#"data-command="POST /api/v2/sources/src1/relocation-plans" data-idempotent="true" data-if-match="&quot;r-2&quot;""#));
    assert!(sources.contains(r#"data-plan-commit="POST /api/v2/sources/src1/relocation""#));
    // Schedules: only tasks the server runs are offered; intervals are minutes.
    assert!(sources.contains(r#"data-command="PUT /api/v2/schedules/sch1" data-idempotent="false" data-if-match="&quot;r-1&quot;""#));
    assert!(sources.contains(r#"data-type="minutes" data-target="interval_seconds""#));
    assert!(sources.contains(r#"value="1440""#));
    assert!(sources.contains(r#"<option value="metadata_refresh">"#));
    assert!(
        !sources.contains(r#"<option value="preview">"#)
            && !sources.contains(r#"<option value="optimize">"#)
    );
    let (_, _, processing) = get_page(&admin, "/processing").await;
    // The server retries failed and cancelled jobs; it takes no precondition.
    assert!(
        processing
            .contains(r#"data-command="POST /api/v2/jobs/job3/retry" data-idempotent="true">"#),
        "{processing}"
    );
    assert!(
        !processing.contains("/api/v2/jobs/job2/retry"),
        "running jobs offer no retry"
    );
}

#[tokio::test]
async fn title_management_names_exact_operations_and_unsupported_parts() {
    let admin = app(Some(principal("everyone", ADMIN)), None);
    let (_, _, item) = get_page(&admin, "/item/item1").await;
    assert!(item.contains(r#"href="/item/item1/manage""#));
    let (status, _, html) = get_page(&admin, "/item/item1/manage").await;
    assert_eq!(status, StatusCode::OK);
    assert_strict_markup(&html);
    // Corrections go to the "local" contribution against the metadata validator.
    assert!(html.contains(r#"data-command="PUT /api/v2/catalog/items/item1/metadata/contributions/local" data-idempotent="false" data-if-match="&quot;r-3&quot;""#), "{html}");
    assert!(html.contains(r#"name="values.title" data-type="optional""#));
    assert!(html.contains(r#"name="values.release_year" data-type="optional-int""#));
    assert!(html.contains(r#"name="locked_fields" data-type="json" value="[]""#));
    // A save replaces the whole local contribution, so unshown local fields ride along.
    assert!(html.contains(r#"name="values" data-type="json" value="{&quot;release_date&quot;:&quot;1999-05-01&quot;}""#), "{html}");
    assert!(html.contains("survive rescans"));
    assert!(html.contains(
        r#"data-command="POST /api/v2/catalog/items/item1/metadata/refresh" data-idempotent="true""#
    ));
    // Merge and split preview first and commit exactly the shown plan.
    assert!(
        html.contains(
            r#"data-command="POST /api/v2/catalog/items/item1/merge-plans" data-idempotent="true""#
        ),
        "{html}"
    );
    assert!(html.contains("&quot;expected_revisions&quot;:{&quot;dup1&quot;:&quot;2&quot;,&quot;item1&quot;:&quot;7&quot;}"), "{html}");
    assert!(html.contains(r#"data-plan-commit="POST /api/v2/catalog/reconciliations""#));
    assert!(html.contains(r#"data-command="POST /api/v2/catalog/items/item1/split-plans""#));
    assert!(
        html.contains(r#"name="version_ids" data-type="member" value="v2""#),
        "{html}"
    );
    // Play orders: created against the item, placements fenced by the
    // timeline and the destination group's revision.
    assert!(html.contains("Aired (aired) · Original"), "{html}");
    assert!(html.contains("position 3 in Aired"), "{html}");
    assert!(html.contains(r#"data-command="POST /api/v2/catalog/items/item1/order-groups" data-idempotent="true" data-if-match="&quot;r-7&quot;""#), "{html}");
    assert!(
        html.contains(r#"data-if-match="&quot;r-2&quot;""#)
            && html.contains(r#"/order" data-idempotent="false""#),
        "{html}"
    );
    assert!(
        html.contains(r#"name="expected_group_revision" data-type="text" value="4""#),
        "{html}"
    );
    assert!(
        html.contains(r#"name="order_position" data-type="number" value="3""#),
        "{html}"
    );
    // Markers: listed with provenance and effect; a new one is checked
    // against the timeline revision the page showed.
    assert!(
        html.contains("Intro 0:05–1:05 · detected, unconfirmed"),
        "{html}"
    );
    assert!(html.contains("not in effect (earlier timeline)"));
    assert!(html.contains(
        r#"data-command="POST /api/v2/catalog/timelines/tl-mock/markers" data-idempotent="true""#
    ));
    assert!(html.contains(r#"name="timeline_revision" data-type="text" value="3""#));
    assert!(html.contains(r#"name="start_ms" data-type="seconds-ms""#));
    // Artwork upload sends the file itself; selection is checked against the item.
    assert!(html.contains(
        r#"data-command="POST /api/v2/catalog/items/item1/artwork" data-idempotent="true""#
    ));
    assert!(html.contains(r#"data-body="file""#));
    // Identification proposes against the exact file revision and library.
    assert!(html.contains(r#"data-command="POST /api/v2/catalog/matches" data-idempotent="true""#));
    assert!(html.contains(r#"value="{&quot;file_id&quot;:&quot;file-item1&quot;,&quot;file_revision&quot;:&quot;rev1&quot;}""#), "{html}");
    // Preparation keeps the chosen audio track and says subtitles are not prepared.
    assert!(
        html.contains(
            r#"data-command="POST /api/v2/processing/preparations" data-idempotent="true""#
        )
    );
    assert!(html.contains(r#"name="tracks.audio_track_id" data-type="nullable""#));
    assert!(html.contains(r#"<option value="a1">"#));
    assert!(html.contains(r#"name="tracks.subtitle_policy" data-type="text" value="off""#));
    assert!(html.contains("subtitles cannot be included in prepared versions yet"));
    // Collection membership is a full replacement guarded by the collection revision.
    assert!(html.contains(r#"data-command="PUT /api/v2/collections/col1" data-idempotent="false" data-if-match="&quot;r-1&quot;" data-body="json""#), "{html}");
    assert!(html.contains(r#"&quot;item_ids&quot;:[&quot;item1&quot;]"#));

    // Each section requires its own permission; a viewer cannot open the page.
    let viewer = app(Some(principal("everyone", VIEWER)), None);
    let (_, _, item) = get_page(&viewer, "/item/item1").await;
    assert!(!item.contains("/item/item1/manage"));
    let (status, _, _) = get_page(&viewer, "/item/item1/manage").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let preparer = app(
        Some(principal(
            "everyone",
            &["catalog:read", "processing:request"],
        )),
        None,
    );
    let (status, _, html) = get_page(&preparer, "/item/item1/manage").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("/api/v2/processing/preparations"));
    assert!(!html.contains("/metadata/contributions/") && !html.contains("/api/v2/collections/"));
    // Restricted titles stay indistinguishable from missing ones.
    let kids = app(Some(principal("kids", ADMIN)), None);
    let (restricted, _, restricted_html) = get_page(&kids, "/item/item3/manage").await;
    let (missing, _, missing_html) = get_page(&kids, "/item/nope/manage").await;
    assert_eq!(restricted, StatusCode::NOT_FOUND);
    assert_eq!(missing, StatusCode::NOT_FOUND);
    assert_eq!(restricted_html, missing_html);
}

#[tokio::test]
async fn collections_page_creates_and_edits_with_exact_bodies() {
    let admin = app(Some(principal("everyone", ADMIN)), None);
    let (_, _, home) = get_page(&admin, "/").await;
    assert!(home.contains(r#"href="/organize""#));
    let (status, _, html) = get_page(&admin, "/organize").await;
    assert_eq!(status, StatusCode::OK);
    assert_strict_markup(&html);
    assert!(html.contains(r#"data-command="POST /api/v2/collections" data-idempotent="true""#));
    assert!(html.contains(r#"name="profile_id" data-type="text" value="everyone""#));
    assert!(html.contains(r#"data-command="POST /api/v2/playlists" data-idempotent="true""#));
    assert!(html.contains(r#"data-command="DELETE /api/v2/collections/col1" data-idempotent="false" data-if-match="&quot;r-1&quot;""#));
    assert!(html.contains("Arrival of the Night Train"));
    // Saved filters: readable conditions, a smart collection from exactly
    // this filter, guarded edits that keep conditions the form cannot show.
    assert!(html.contains("Unwatched 90s films"));
    assert!(
        html.contains("Kind is movie · From 1990 · Person p7"),
        "{html}"
    );
    assert!(html.contains(r#"data-command="POST /api/v2/catalog/filters" data-idempotent="true""#));
    assert!(html.contains(r#"data-command="PUT /api/v2/catalog/filters/flt1" data-idempotent="false" data-if-match="&quot;r-1&quot;""#));
    assert!(html.contains(r#"data-command="DELETE /api/v2/catalog/filters/flt1""#));
    assert!(
        html.contains("&quot;kind&quot;:&quot;smart&quot;")
            && html.contains("&quot;filter_id&quot;:&quot;flt1&quot;"),
        "{html}"
    );
    assert!(
        html.contains(r#"<option value="movie" selected="">"#),
        "{html}"
    );
    assert!(
        html.contains(
            r#"data-field="year" data-operator="gte" data-value-type="int" value="1990""#
        ),
        "{html}"
    );
    assert!(html.contains(r#"type="hidden" name="all" data-type="term" data-field="person_id" data-operator="eq" data-value-type="text" value="p7""#), "{html}");
    let viewer = app(Some(principal("everyone", VIEWER)), None);
    let (status, _, _) = get_page(&viewer, "/organize").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn downloads_are_requested_and_cancelled_through_public_operations() {
    let viewer = app(Some(principal("everyone", VIEWER)), None);
    let (status, _, _) = get_page(&viewer, "/downloads").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, _, item) = get_page(&viewer, "/item/item1").await;
    assert!(
        !item.contains("/api/v2/downloads"),
        "no download action without downloads:manage"
    );

    let mut permissions = VIEWER.to_vec();
    permissions.push("downloads:manage");
    let saver = app(Some(principal("everyone", &permissions)), None);
    let (status, _, page) = get_page(&saver, "/downloads").await;
    assert_eq!(status, StatusCode::OK);
    assert_strict_markup(&page);
    assert!(page.contains(r#"data-command="DELETE /api/v2/downloads/download1""#));
    assert!(page.contains("Ready to save"));
    let (_, _, home) = get_page(&saver, "/").await;
    assert!(home.contains(r#"href="/downloads""#));
    let (_, _, item) = get_page(&saver, "/item/item1").await;
    // Hidden until the desktop host reveals it; the body carries every
    // required DownloadInput field for this device, profile and timeline.
    let action = &item[item.find("data-desktop-only").expect("download action")..];
    let action = &action[..action.find("</form>").unwrap()];
    assert!(action.contains(r#"hidden"#));
    assert!(action.contains(r#"data-command="POST /api/v2/downloads" data-idempotent="true""#));
    for field in [
        r#"name="device_id" data-type="text" value="p1""#,
        r#"name="profile_id" data-type="text" value="everyone""#,
        r#"name="timeline_id" data-type="text" value="tl1""#,
        r#"name="version_id" data-type="json" value="null""#,
        r#"name="recipe_id" data-type="json" value="null""#,
        r#"name="tracks" data-type="json""#,
    ] {
        assert!(action.contains(field), "{field}");
    }
}
