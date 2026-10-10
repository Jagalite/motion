//! Topcoat pages. Server-rendered only: no `topcoat::runtime`, no shards,
//! procedures or inline scripts. Every page derives its data from the request
//! principal through `UiQueryFacade`; rendering has no side effects, so it is
//! safe to repeat.

use topcoat::{
    Result,
    context::{Cx, app_context},
    router::{
        Router, Slot, StatusCode,
        error::{
            ForbiddenError, NotFoundError, ServiceUnavailableError, UnauthorizedError, forbidden,
            not_found, service_unavailable, unauthorized,
        },
        href, layout, page, path_param, query_params,
        request::extensions,
    },
    view::{View, component, error_boundary, view},
};

use crate::{
    assets,
    facade::{Availability, Facade, ItemCard, UiError, UiPrincipal},
};

path_param!(library_id);
path_param!(item_id);
path_param!(timeline_id);

#[query_params(error = redirect("?"))]
struct SearchQuery {
    q: Option<String>,
}

pub fn router(facade: Facade) -> Router {
    Router::builder()
        .app_context(facade)
        .layout(shell)
        .page(home)
        .page(library)
        .page(item)
        .page(search)
        .page(play)
        .page(crate::screens::profiles)
        .page(crate::screens::sources)
        .page(crate::screens::matches)
        .page(crate::screens::processing)
        .page(crate::screens::diagnostics)
        .build()
}

/// The authenticated principal placed in request extensions by the host.
pub(crate) fn principal(cx: &Cx) -> Result<&UiPrincipal> {
    extensions(cx)
        .get::<UiPrincipal>()
        .ok_or_else(|| unauthorized().into())
}

pub(crate) fn facade(cx: &Cx) -> &Facade {
    app_context::<Facade>(cx)
}

pub(crate) fn ui_error(error: UiError) -> topcoat::Error {
    match error {
        UiError::NotFound => not_found().into(),
        UiError::Denied => forbidden().into(),
        UiError::Unavailable => service_unavailable(5).into(),
    }
}

fn clock(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub(crate) fn availability_text(value: Availability) -> &'static str {
    match value {
        Availability::Available => "Available",
        Availability::Unavailable => "Offline",
        Availability::Unknown => "Not yet verified",
        Availability::Degraded => "Partly available",
    }
}

// --- Layout -----------------------------------------------------------------

#[layout("/")]
async fn shell(cx: &Cx, slot: Slot<'_>) -> Result<impl View> {
    let mock = facade(cx).0.is_mock();
    let who = extensions(cx).get::<UiPrincipal>().cloned();
    Ok(view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width,initial-scale=1">
                <meta name="referrer" content="no-referrer">
                if let Some(who) = &who {
                    <meta name="motion-csrf" content=(who.csrf_token.as_str())>
                    <meta name="motion-server-epoch" content=(who.server_epoch.as_str())>
                }
                <title>"Motion"</title>
                <link rel="stylesheet" href=(assets::STYLESHEET.url())>
                <script type="module" src=(assets::BRIDGE.url())></script>
            </head>
            <body>
                <a class="skip-link" href="#main">"Skip to content"</a>
                if mock {
                    <div class="mock-banner" role="note">
                        "Development mock — not a Motion server. Nothing here is real library data or evidence of server behaviour."
                    </div>
                }
                <header class="app-header">
                    <a class="brand" href=(href!(home))>"Motion"</a>
                    if let Some(who) = &who {
                        <nav aria-label="Main">
                            <ul>
                                <li><a href=(href!(home))>"Home"</a></li>
                                if who.can("catalog:write") { <li><a href=(href!(crate::screens::matches))>"Matches"</a></li> }
                                if who.can("processing:request") { <li><a href=(href!(crate::screens::processing))>"Processing"</a></li> }
                                if who.can("sources:manage") { <li><a href=(href!(crate::screens::sources))>"Sources"</a></li> }
                                <li><a href=(href!(crate::screens::profiles))>"Profiles"</a></li>
                                if who.can("system:admin") { <li><a href=(href!(crate::screens::diagnostics))>"Diagnostics"</a></li> }
                            </ul>
                        </nav>
                        <form role="search" class="search" method="get" action=(href!(search))>
                            <label class="visually-hidden" for="global-search">"Search the catalog"</label>
                            <input id="global-search" type="search" name="q" placeholder="Search">
                        </form>
                        if who.profiles.len() > 1 {
                            <label class="profile">"Profile "
                                <select data-profile-switch="motion_profile">
                                    for option in who.profiles.iter() {
                                        <option value=(option.id.as_str()) selected=(option.id == who.profile_id)>(option.name.as_str())</option>
                                    }
                                </select>
                            </label>
                        } else {
                            <span class="profile">"Profile: " (who.profile_name.as_str())</span>
                        }
                    }
                </header>
                <main id="main" tabindex="-1">
                    error_boundary(
                        fallback: |error| {
                            let (status, title, message) = if error.downcast_ref::<UnauthorizedError>().is_some() {
                                (StatusCode::UNAUTHORIZED, "Sign in to Motion", "Your session has ended or this browser is not paired. Sign in from the Motion app or pair this browser.")
                            } else if error.downcast_ref::<ForbiddenError>().is_some() {
                                (StatusCode::FORBIDDEN, "You don’t have access", "This profile or device is not allowed to view this.")
                            } else if error.downcast_ref::<NotFoundError>().is_some() {
                                (StatusCode::NOT_FOUND, "Nothing here", "This no longer exists or is not visible to you.")
                            } else if error.downcast_ref::<ServiceUnavailableError>().is_some() {
                                (StatusCode::SERVICE_UNAVAILABLE, "Server unavailable", "The server is busy or still starting. Try again shortly.")
                            } else {
                                return Err(error);
                            };
                            let sign_in = status == StatusCode::UNAUTHORIZED;
                            Ok(view! {
                                (status)
                                <section class="status-panel" role="alert" data-state=(status.as_u16())>
                                    <h1>(title)</h1>
                                    <p>(message)</p>
                                    if sign_in {
                                        crate::screens::pairing_form()
                                    }
                                </section>
                            })
                        },
                        (slot)
                    )
                </main>
            </body>
        </html>
    })
}

// --- Components -------------------------------------------------------------

#[component]
async fn item_grid(items: Vec<ItemCard>, label: &str) -> Result<impl View> {
    Ok(view! {
        <ul class="grid" aria-label=(label)>
            for entry in items {
                <li class="card">
                    <a href=(href!(item, ItemId(entry.id.as_str())))>
                        <span class="card-title">(entry.title.as_str())</span>
                        <span class="card-meta">
                            (entry.kind.as_str())
                            if entry.availability != Availability::Available {
                                " · " (availability_text(entry.availability))
                            }
                            if entry.needs_review { " · needs review" }
                        </span>
                    </a>
                </li>
            }
        </ul>
    })
}

#[component]
pub(crate) async fn empty(message: &str) -> Result<impl View> {
    Ok(view! { <p class="status-panel" role="status" data-state="empty">(message)</p> })
}

// --- Pages ------------------------------------------------------------------

#[page("/")]
async fn home(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let view_model = facade(cx).0.home(who).await.map_err(ui_error)?;
    Ok(view! {
        <h1>"Home"</h1>
        <section aria-labelledby="continue-heading">
            <h2 id="continue-heading">"Continue watching"</h2>
            if !view_model.continue_available {
                empty(message: "Continue watching is not available on this server yet.")
            } else if view_model.continue_watching.is_empty() {
                empty(message: "Nothing in progress for this profile.")
            } else {
                <ul class="grid" aria-label="Continue watching">
                    for entry in view_model.continue_watching {
                        <li class="card">
                            <a href=(href!(play, TimelineId(entry.timeline_id.as_str())))>
                                <span class="card-title">(entry.item.title.as_str())</span>
                                <span class="card-meta">"Resume at " (clock(entry.position_ms))</span>
                                if let Some(duration) = entry.duration_ms {
                                    <progress max=(duration) value=(entry.position_ms)
                                        aria-label=(format!("{}% watched", entry.position_ms * 100 / duration.max(1)))></progress>
                                }
                            </a>
                        </li>
                    }
                </ul>
            }
        </section>
        <section aria-labelledby="libraries-heading">
            <h2 id="libraries-heading">"Libraries"</h2>
            if view_model.libraries.is_empty() {
                empty(message: "No libraries yet.")
            } else {
                <ul class="grid" aria-label="Libraries">
                    for lib in view_model.libraries {
                        <li class="card">
                            <a href=(href!(library, LibraryId(lib.id.as_str())))>
                                <span class="card-title">(lib.name.as_str())</span>
                                <span class="card-meta">(lib.kind.as_str()) " · " (availability_text(lib.availability))</span>
                            </a>
                        </li>
                    }
                </ul>
            }
        </section>
    })
}

#[page("/library/{library_id}")]
async fn library(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let id = path_param::<LibraryId>(cx);
    let view_model = facade(cx).0.library(who, id).await.map_err(ui_error)?;
    let degraded = view_model.library.availability == Availability::Degraded;
    Ok(view! {
        <h1>(view_model.library.name.as_str())</h1>
        if degraded {
            <p class="notice" role="note">"Some sources of this library are offline. Their titles stay listed but cannot play until the source returns."</p>
        }
        if view_model.items.is_empty() {
            empty(message: "This library has no titles yet. A scan may still be running.")
        } else {
            item_grid(items: view_model.items, label: "Titles")
        }
    })
}

#[page("/item/{item_id}")]
async fn item(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let id = path_param::<ItemId>(cx);
    let view_model = facade(cx).0.item(who, id).await.map_err(ui_error)?;
    let can_play = who.can("playback:request") && view_model.playback_available;
    Ok(view! {
        <h1>(view_model.item.title.as_str())</h1>
        <p class="item-meta">
            (view_model.item.kind.as_str()) " · " (availability_text(view_model.item.availability))
            if view_model.item.needs_review { " · identification needs review" }
        </p>
        if !view_model.children.is_empty() {
            item_grid(items: view_model.children, label: "Entries")
        }
        if !view_model.playback_available {
            <p class="muted">"Playback is unavailable."</p>
        }
        for timeline in view_model.timelines {
            <section class="timeline" aria-labelledby=(format!("tl-{}", timeline.id))>
                <h2 id=(format!("tl-{}", timeline.id))>
                    (timeline.edition.as_str())
                    if let Some(duration) = timeline.duration_ms { " · " (clock(duration)) }
                </h2>
                if let Some(viewing) = &timeline.viewing {
                    if viewing.watched {
                        <p>"Watched."</p>
                    } else if viewing.position_ms > 0 {
                        <p>"Stopped at " (clock(viewing.position_ms)) "."</p>
                    } else {
                        <p>"Not started."</p>
                    }
                } else {
                    <p>"Viewing history is unavailable."</p>
                }
                if can_play {
                    <p><a class="button primary" href=(href!(play, TimelineId(timeline.id.as_str())))>
                        if timeline.viewing.as_ref().is_some_and(|v| v.position_ms > 0 && !v.watched) { "Resume" } else { "Play" }
                    </a></p>
                }
                <h3>"Versions"</h3>
                <ul class="versions">
                    for version in timeline.versions {
                        <li>
                            <span class="version-label">(version.label.as_str())</span>
                            " · " (availability_text(version.availability))
                            <div class="muted small">(version.summary.as_str())</div>
                        </li>
                    }
                </ul>
            </section>
        }
    })
}

#[page("/search")]
async fn search(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let query = query_params::<SearchQuery>(cx)?;
    let q = query.q.as_deref().unwrap_or("");
    if q.len() > 200 {
        return Err(topcoat::router::error::bad_request("search text is too long").into());
    }
    let view_model = facade(cx).0.search(who, q).await.map_err(ui_error)?;
    let heading = if view_model.query.is_empty() {
        "Search".to_string()
    } else {
        format!("Results for “{}”", view_model.query)
    };
    Ok(view! {
        <h1>(heading.as_str())</h1>
        if view_model.query.is_empty() {
            empty(message: "Type in the search box to find titles.")
        } else if view_model.items.is_empty() {
            empty(message: "No titles match.")
        } else {
            item_grid(items: view_model.items, label: "Search results")
        }
    })
}

/// Emits a stable player host. The external playback module owns everything
/// inside `#motion-player`; this page is never partially re-rendered.
#[page("/play/{timeline_id}")]
async fn play(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let id = path_param::<TimelineId>(cx);
    let view_model = facade(cx).0.player(who, id).await.map_err(ui_error)?;
    Ok(view! {
        <h1>(view_model.title.as_str())</h1>
        <div id="motion-player" class="player-host"
            data-motion-player="v1"
            data-timeline-id=(view_model.timeline_id.as_str())
            data-profile-id=(who.profile_id.as_str())
            data-viewing-revision=(view_model.viewing_revision.as_str())
            data-resume-ms=(view_model.resume_ms)
            data-duration-ms=(view_model.duration_ms.unwrap_or(0))
            data-demuxe-base=(assets::DEMUXE_BASE)>
            <p class="status-panel" role="status" data-state="loading">"Preparing playback…"</p>
        </div>
        <script type="module" src=(assets::PLAYER.url())></script>
        <p><a href=(href!(item, ItemId(view_model.item_id.as_str())))>"Back to title"</a></p>
    })
}
