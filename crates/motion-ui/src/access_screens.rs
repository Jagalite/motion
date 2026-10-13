//! Authentication pages use the public, independently authorized API. The page
//! gate and navigation are presentation conveniences; no authorization decision
//! or persistent authentication state lives in the browser.
use topcoat::{
    Result,
    context::Cx,
    router::{error::forbidden, page},
    view::{View, view},
};

use crate::{app::principal, assets};

#[page("/settings/access")]
pub(crate) async fn access(cx: &Cx) -> Result<impl View> {
    if !principal(cx)?.can("system:admin") {
        return Err(forbidden().into());
    }
    Ok(view! {
        <div class="access-page" data-access-page="admin">
            <header class="page-header"><h1 class="page-title">"Authentication & Access"</h1></header>
            <p class="muted">"Manage sign-in methods, people and the devices allowed to use this server."</p>
            <nav class="access-tabs" aria-label="Authentication and access sections">
                <a href="#overview">"Overview"</a>
                <a href="#methods">"Sign-in methods"</a>
                <a href="#users">"Users"</a>
                <a href="#requests">"Access requests"</a>
                <a href="#enrollment">"Pairing & invitations"</a>
                <a href="#identities">"Linked identities"</a>
                <a href="#sessions">"Devices & sessions"</a>
                <a href="#activity">"Security activity"</a>
            </nav>
            <p data-access-status="" role="status" aria-live="polite">"Loading access settings…"</p>
            <div data-access-content=""></div>
            <noscript><p class="notice">"JavaScript is required to manage access. You can also use the Motion administrator terminal."</p></noscript>
        </div>
        <script type="module" src=(assets::AUTH.url())></script>
    })
}

#[page("/settings/signin")]
pub(crate) async fn my_signin(cx: &Cx) -> Result<impl View> {
    principal(cx)?;
    Ok(view! {
        <div class="access-page" data-access-page="self">
            <header class="page-header"><h1 class="page-title">"My sign-in methods"</h1></header>
            <p class="muted">"Manage the methods linked to your account and your browser sessions."</p>
            <p data-access-status="" role="status" aria-live="polite">"Loading your sign-in methods…"</p>
            <div data-access-content=""></div>
            <noscript><p class="notice">"JavaScript is required to manage sign-in methods."</p></noscript>
        </div>
        <script type="module" src=(assets::AUTH.url())></script>
    })
}

#[page("/signin")]
pub(crate) async fn signin(cx: &Cx) -> Result<impl View> {
    let _ = cx;
    Ok(view! {
        <div class="access-page access-signin" data-access-page="signin">
            <header class="page-header"><h1 class="page-title">"Sign in to Motion"</h1></header>
            <p class="muted">"Choose a sign-in method configured by this server’s administrator."</p>
            <p data-access-status="" role="status" aria-live="polite">"Loading sign-in options…"</p>
            <div data-access-content=""></div>
            <section class="access-section" aria-labelledby="pair-this-browser">
                <h2 id="pair-this-browser">"Pair this browser"</h2>
                <p>"Ask an administrator to approve this browser from an authorized Motion device."</p>
                crate::screens::pairing_form()
            </section>
            <noscript><p class="notice">"Enable JavaScript to sign in or pair this browser."</p></noscript>
        </div>
        <script type="module" src=(assets::AUTH.url())></script>
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        Extension, Router,
        body::Body,
        http::{Request, StatusCode, header},
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::{UiPrincipal, assets, facade::ProfileOption, mock::MockUiQueryFacade};

    fn app(permissions: Option<&[&str]>) -> Router {
        let app = crate::mount(Router::new(), Arc::new(MockUiQueryFacade), None);
        match permissions {
            None => app,
            Some(permissions) => app.layer(Extension(UiPrincipal {
                principal_id: "device-1".into(),
                profile_id: "everyone".into(),
                profile_name: "Everyone".into(),
                profiles: vec![ProfileOption {
                    id: "everyone".into(),
                    name: "Everyone".into(),
                }],
                permissions: permissions.iter().map(|value| (*value).into()).collect(),
                server_epoch: "epoch-1".into(),
                viewing_epoch: "server:restore".into(),
                csrf_token: "session-csrf".into(),
            })),
        }
    }

    async fn read(app: Router, path: &str) -> (StatusCode, String) {
        let response = app
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, no-store"
        );
        assert_eq!(
            response.headers()["content-security-policy"],
            crate::CONTENT_SECURITY_POLICY
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn anonymous_signin_is_public_but_access_pages_require_identity() {
        let (status, html) = read(app(None), "/signin").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("data-access-page=\"signin\""));
        assert!(html.contains(&assets::AUTH.url()));
        assert!(!html.contains("motion-csrf"));
        for path in ["/settings/access", "/settings/signin"] {
            assert_eq!(read(app(None), path).await.0, StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn administrator_screen_and_navigation_require_system_admin() {
        let viewer = Some(&["catalog:read"][..]);
        let (status, html) = read(app(viewer), "/settings/access").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(!html.contains("data-access-page=\"admin\""));
        assert!(!html.contains("href=\"/settings/access\""));
        let (status, html) = read(app(viewer), "/settings/signin").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("data-access-page=\"self\""));

        let (status, html) = read(app(Some(&["system:admin"])), "/settings/access").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("data-access-page=\"admin\""));
        assert!(html.contains("href=\"/settings/access\""));
        assert!(html.contains("content=\"session-csrf\""));
        for section in [
            "overview",
            "methods",
            "users",
            "requests",
            "enrollment",
            "identities",
            "sessions",
            "activity",
        ] {
            assert!(html.contains(&format!("href=\"#{section}\"")));
        }
        assert!(!html.contains(" onclick="));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("style=\""));
    }
}
