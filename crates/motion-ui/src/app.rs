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
        request::{extensions, uri},
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
        .page(library_index)
        .page(manage)
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
    extensions(cx)
        .get::<Facade>()
        .unwrap_or_else(|| app_context::<Facade>(cx))
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

fn kind_label(kind: &str) -> &str {
    match kind {
        "movie" => "Movie",
        "series" => "Series",
        "season" => "Season",
        "episode" => "Episode",
        "movies" => "Movies",
        "shows" | "television" | "tv" => "TV shows",
        other => other,
    }
}

/// Placeholder artwork tone until the facade exposes real artwork. Stable per
/// item so a title keeps its color between pages. Classes, not inline styles,
/// because the presentation CSP admits no style attributes.
fn art_tone(id: &str) -> &'static str {
    const TONES: [&str; 8] = [
        "art tone-0",
        "art tone-1",
        "art tone-2",
        "art tone-3",
        "art tone-4",
        "art tone-5",
        "art tone-6",
        "art tone-7",
    ];
    let sum = id.bytes().fold(0u32, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(u32::from(b))
    });
    TONES[(sum % 8) as usize]
}

/// Up to two initials from the significant words of a title.
fn initials(title: &str) -> String {
    title
        .split_whitespace()
        .filter(|word| {
            !matches!(
                word.to_lowercase().as_str(),
                "the" | "a" | "an" | "of" | "and"
            )
        })
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

fn library_icon(kind: &str) -> &'static str {
    match kind {
        "movies" => "movies",
        "shows" | "television" | "tv" => "shows",
        _ => "library",
    }
}

fn icon_path(name: &str) -> &'static str {
    match name {
        "home" => "M3 10.5 12 3l9 7.5V20a1 1 0 0 1-1 1h-5v-6H9v6H4a1 1 0 0 1-1-1z",
        "movies" => "M4 4h16v16H4zM8 4v16M16 4v16M4 8h4M4 12h4M4 16h4M16 8h4M16 12h4M16 16h4",
        "shows" => "M3 6h18v12H3zM8 21h8M12 18v3",
        "library" => "M4 4h4v16H4zM10 4h4v16h-4zM16 5l3.5-.8 3 15.6-3.5.8z",
        "search" => "M11 4a7 7 0 1 1 0 14 7 7 0 0 1 0-14zM16.5 16.5 21 21",
        "play" => "M7 4.5v15l12.5-7.5z",
        "check" => "M5 12.5 10 17.5 19.5 7",
        "matches" => "M4 7h10M4 12h16M4 17h7M17 4l3 3-3 3",
        "processing" => {
            "M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M5.6 18.4l2.1-2.1M16.3 7.7l2.1-2.1"
        }
        "sources" => {
            "M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3zM4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"
        }
        "profiles" => "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM4 21a8 8 0 0 1 16 0",
        "diagnostics" => "M3 12h4l3-8 4 16 3-8h4",
        "manage" => "M4 6h9M17 6h3M4 12h3M11 12h9M4 18h11M19 18h1M15 4v4M9 10v4M17 16v4",
        "pause" => "M8 5v14M16 5v14",
        "volume" => "M4 9.5h3.5L12 5.5v13l-4.5-4H4zM15.5 9a4 4 0 0 1 0 6M18 6.5a7.5 7.5 0 0 1 0 11",
        "next" => "M5 5.5v13l9-6.5zM18 5.5v13",
        "back" => "M15 5l-7 7 7 7",
        "chevron" => "M9 5l7 7-7 7",
        _ => "M12 12h.01",
    }
}

#[component]
async fn icon(name: &'static str) -> Result<impl View> {
    Ok(view! {
        <svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
            <path d=(icon_path(name))></path>
        </svg>
    })
}

#[component]
async fn nav_link(
    href: String,
    label: String,
    icon_name: &'static str,
    current: bool,
) -> Result<impl View> {
    Ok(view! {
        <li>
            <a class=(if current { "nav-link is-current" } else { "nav-link" }) href=(href.as_str())
                aria-current=(if current { "page" } else { "false" })>
                icon(name: icon_name)
                <span class="nav-label">(label.as_str())</span>
            </a>
        </li>
    })
}

#[component]
async fn tab_link(
    href: String,
    label: &'static str,
    icon_name: &'static str,
    current: bool,
) -> Result<impl View> {
    Ok(view! {
        <a class=(if current { "tab-link is-current" } else { "tab-link" }) href=(href.as_str())
            aria-current=(if current { "page" } else { "false" })>
            icon(name: icon_name)
            <span class="tab-label">(label)</span>
        </a>
    })
}

struct ManageLink {
    href: &'static str,
    label: &'static str,
    icon_name: &'static str,
}

/// Administrative screens this principal may open, in navigation order.
fn manage_links(who: &UiPrincipal) -> Vec<ManageLink> {
    [
        ("catalog:write", "/matches", "Matches", "matches"),
        (
            "processing:request",
            "/processing",
            "Processing",
            "processing",
        ),
        ("sources:manage", "/sources", "Sources", "sources"),
        ("system:admin", "/diagnostics", "Diagnostics", "diagnostics"),
    ]
    .into_iter()
    .filter(|(permission, ..)| who.can(permission))
    .map(|(_, href, label, icon_name)| ManageLink {
        href,
        label,
        icon_name,
    })
    .collect()
}

// --- Layout -----------------------------------------------------------------

#[layout("/")]
async fn shell(cx: &Cx, slot: Slot<'_>) -> Result<impl View> {
    let mock = facade(cx).0.is_mock();
    let who = extensions(cx).get::<UiPrincipal>().cloned();
    let path = uri(cx).path().to_string();
    // The sidebar lists the principal's libraries. The read is side-effect free;
    // if it fails the page still renders and only the list is omitted.
    let libraries = match &who {
        Some(who) => facade(cx)
            .0
            .home(who)
            .await
            .map(|view| view.libraries)
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let can_manage = who
        .as_ref()
        .is_some_and(|who| !manage_links(who).is_empty());
    let initial = who
        .as_ref()
        .and_then(|who| who.profile_name.chars().next())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    Ok(view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
                <meta name="referrer" content="no-referrer">
                <meta name="color-scheme" content="dark">
                <meta name="theme-color" content="#191c1f">
                <meta name="mobile-web-app-capable" content="yes">
                <meta name="apple-mobile-web-app-capable" content="yes">
                <meta name="apple-mobile-web-app-status-bar-style" content="black-translucent">
                <meta name="apple-mobile-web-app-title" content="Motion">
                <link rel="manifest" href=(assets::MANIFEST_URL)>
                <link rel="apple-touch-icon" href=(assets::TOUCH_ICON.url())>
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
                <div class="shell">
                    <aside class="sidebar">
                        <a class="brand" href=(href!(home))>
                            <span class="brand-mark" aria-hidden="true">
                                <svg viewBox="0 0 24 24" focusable="false"><path d="M9 6.5v11l8.5-5.5z"></path></svg>
                            </span>
                            <span class="brand-name">"Motion"</span>
                        </a>
                        if let Some(who) = &who {
                            <nav class="sidebar-nav" aria-label="Main">
                                <ul class="nav-list">
                                    nav_link(href: href!(home).resolve(cx), label: "Home".to_string(), icon_name: "home", current: path == "/")
                                </ul>
                                if !libraries.is_empty() {
                                    <h2 class="nav-heading" id="nav-libraries">"Libraries"</h2>
                                    <ul class="nav-list" aria-labelledby="nav-libraries">
                                        for lib in libraries.iter() {
                                            nav_link(
                                                href: href!(library, LibraryId(lib.id.as_str())).resolve(cx),
                                                label: lib.name.clone(),
                                                icon_name: library_icon(&lib.kind),
                                                current: path == format!("/library/{}", lib.id),
                                            )
                                        }
                                    </ul>
                                }
                                if can_manage {
                                    <h2 class="nav-heading" id="nav-manage">"Manage"</h2>
                                    <ul class="nav-list" aria-labelledby="nav-manage">
                                        for link in manage_links(who) {
                                            nav_link(href: link.href.to_string(), label: link.label.to_string(), icon_name: link.icon_name, current: path.starts_with(link.href))
                                        }
                                    </ul>
                                }
                                <ul class="nav-list nav-foot">
                                    nav_link(href: href!(crate::screens::profiles).resolve(cx), label: "Profiles".to_string(), icon_name: "profiles", current: path.starts_with("/profiles"))
                                </ul>
                            </nav>
                        }
                    </aside>
                    <div class="content">
                        if mock {
                            <div class="mock-banner" role="note">
                                "Development mock — not a Motion server. Nothing here is real library data or evidence of server behaviour."
                            </div>
                        }
                        if let Some(who) = &who {
                            <header class="topbar">
                                <a class="topbar-brand" href=(href!(home)) aria-label="Motion home">
                                    <span class="brand-mark" aria-hidden="true">
                                        <svg viewBox="0 0 24 24" focusable="false"><path d="M9 6.5v11l8.5-5.5z"></path></svg>
                                    </span>
                                    <span class="brand-name" aria-hidden="true">"Motion"</span>
                                </a>
                                <form role="search" class="search" method="get" action=(href!(search))>
                                    <label class="visually-hidden" for="global-search">"Search the catalog"</label>
                                    icon(name: "search")
                                    <input id="global-search" type="search" name="q" placeholder="Search titles" autocomplete="off">
                                </form>
                                <div class="topbar-profile">
                                    if who.profiles.len() > 1 {
                                        <label class="profile">
                                            <span class="visually-hidden">"Profile"</span>
                                            <select data-profile-switch="motion_profile">
                                                for option in who.profiles.iter() {
                                                    <option value=(option.id.as_str()) selected=(option.id == who.profile_id)>(option.name.as_str())</option>
                                                }
                                            </select>
                                        </label>
                                    } else {
                                        <span class="profile">"Profile: " (who.profile_name.as_str())</span>
                                    }
                                    <a class="avatar" href=(href!(crate::screens::profiles)) aria-label=(format!("Profile: {}", who.profile_name))>(initial.as_str())</a>
                                </div>
                            </header>
                        }
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
                    </div>
                    if who.is_some() {
                        // Phone navigation: a fixed set of tabs, like a native app.
                        // CSS shows either this or the sidebar, never both.
                        <nav class="tabbar" aria-label="Tabs">
                            tab_link(href: href!(home).resolve(cx), label: "Home", icon_name: "home", current: path == "/")
                            tab_link(href: href!(library_index).resolve(cx), label: "Libraries", icon_name: "library",
                                current: path.starts_with("/librar"))
                            tab_link(href: href!(search).resolve(cx), label: "Search", icon_name: "search", current: path.starts_with("/search"))
                            if can_manage {
                                tab_link(href: href!(manage).resolve(cx), label: "Manage", icon_name: "manage",
                                    current: path.starts_with("/manage") || ["/matches", "/processing", "/sources", "/diagnostics"].iter().any(|p| path.starts_with(p)))
                            }
                            tab_link(href: href!(crate::screens::profiles).resolve(cx), label: "Profile", icon_name: "profiles", current: path.starts_with("/profiles"))
                        </nav>
                    }
                </div>
            </body>
        </html>
    })
}

// --- Components -------------------------------------------------------------

#[component]
async fn poster(id: String, title: String) -> Result<impl View> {
    let letters = initials(&title);
    Ok(view! {
        <span class=(art_tone(&id)) aria-hidden="true">
            <span class="art-initials">(letters.as_str())</span>
        </span>
    })
}

#[component]
async fn item_grid(items: Vec<ItemCard>, label: &str) -> Result<impl View> {
    Ok(view! {
        <ul class="poster-grid" aria-label=(label)>
            for entry in items {
                <li class="poster-card">
                    <a href=(href!(item, ItemId(entry.id.as_str())))>
                        <span class="poster-frame">
                            poster(id: entry.id.clone(), title: entry.title.clone())
                            if entry.needs_review {
                                <span class="badge badge-review">"Needs review"</span>
                            }
                        </span>
                        <span class="card-title">(entry.title.as_str())</span>
                        <span class="card-meta">
                            (kind_label(&entry.kind))
                            if entry.availability != Availability::Available {
                                <span class="meta-sep" aria-hidden="true"></span>
                                <span class="availability">(availability_text(entry.availability))</span>
                            }
                            if entry.needs_review { <span class="visually-hidden">", needs review"</span> }
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
        <h1 class="page-title">"Home"</h1>
        <section class="shelf" aria-labelledby="continue-heading">
            <h2 class="shelf-title" id="continue-heading">"Continue watching"</h2>
            if !view_model.continue_available {
                empty(message: "Continue watching is not available on this server yet.")
            } else if view_model.continue_watching.is_empty() {
                empty(message: "Nothing in progress for this profile.")
            } else {
                <ul class="shelf-row" aria-label="Continue watching">
                    for entry in view_model.continue_watching {
                        <li class="wide-card">
                            <a href=(href!(play, TimelineId(entry.timeline_id.as_str())))>
                                <span class="wide-frame">
                                    poster(id: entry.item.id.clone(), title: entry.item.title.clone())
                                    <span class="play-badge" aria-hidden="true">icon(name: "play")</span>
                                    if let Some(duration) = entry.duration_ms {
                                        <progress max=(duration) value=(entry.position_ms)
                                            aria-label=(format!("{}% watched", entry.position_ms * 100 / duration.max(1)))></progress>
                                    }
                                </span>
                                <span class="card-title">(entry.item.title.as_str())</span>
                                <span class="card-meta">"Resume at " (clock(entry.position_ms))
                                    if let Some(duration) = entry.duration_ms {
                                        <span class="meta-sep" aria-hidden="true"></span>
                                        (clock(duration.saturating_sub(entry.position_ms))) " left"
                                    }
                                </span>
                            </a>
                        </li>
                    }
                </ul>
            }
        </section>
        <section class="shelf" aria-labelledby="libraries-heading">
            <h2 class="shelf-title" id="libraries-heading">"Libraries"</h2>
            if view_model.libraries.is_empty() {
                empty(message: "No libraries yet.")
            } else {
                <ul class="library-row" aria-label="Libraries">
                    for lib in view_model.libraries {
                        <li class="library-card">
                            <a href=(href!(library, LibraryId(lib.id.as_str())))>
                                <span class=(art_tone(&lib.id)) aria-hidden="true">
                                    icon(name: library_icon(&lib.kind))
                                </span>
                                <span class="card-title">(lib.name.as_str())</span>
                                <span class="card-meta">(kind_label(&lib.kind))
                                    <span class="meta-sep" aria-hidden="true"></span>
                                    <span class=(if lib.availability == Availability::Available { "availability is-ok" } else { "availability" })>(availability_text(lib.availability))</span>
                                </span>
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
    let count = view_model.items.len();
    Ok(view! {
        <a class="back-link" href=(href!(library_index))>icon(name: "back") "Libraries"</a>
        <header class="page-header">
            <h1 class="page-title">(view_model.library.name.as_str())</h1>
            <p class="page-count">(count) if count == 1 { " title" } else { " titles" }</p>
        </header>
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
    let title = view_model.item.title.clone();
    let entries_heading = match view_model.item.kind.as_str() {
        "series" => "Seasons",
        "season" => "Episodes",
        _ => "Entries",
    };
    Ok(view! {
        <a class="back-link" href=(href!(home)) data-back="history">icon(name: "back") "Back"</a>
        <article class="detail">
            <div class=(format!("detail-backdrop {}", art_tone(&view_model.item.id))) aria-hidden="true"></div>
            <div class="detail-hero">
                <div class="detail-poster">
                    poster(id: view_model.item.id.clone(), title: title.clone())
                </div>
                <div class="detail-info">
                    <h1 class="detail-title">(title.as_str())</h1>
                    <p class="item-meta">
                        <span class="meta-kind">(kind_label(&view_model.item.kind))</span>
                        <span class="meta-sep" aria-hidden="true"></span>
                        <span class=(if view_model.item.availability == Availability::Available { "availability is-ok" } else { "availability" })>(availability_text(view_model.item.availability))</span>
                        if view_model.item.needs_review {
                            <span class="badge badge-review">"Identification needs review"</span>
                        }
                    </p>
                    if !view_model.playback_available {
                        <p class="muted">"Playback is unavailable."</p>
                    }
                    for timeline in view_model.timelines.iter() {
                        <section class="edition" aria-labelledby=(format!("tl-{}", timeline.id))>
                            <h2 class="edition-title" id=(format!("tl-{}", timeline.id))>
                                (timeline.edition.as_str())
                                if let Some(duration) = timeline.duration_ms {
                                    <span class="edition-runtime">(clock(duration))</span>
                                }
                            </h2>
                            <div class="edition-actions">
                                if can_play {
                                    <a class="button primary" href=(href!(play, TimelineId(timeline.id.as_str())))>
                                        icon(name: "play")
                                        if timeline.viewing.as_ref().is_some_and(|v| v.position_ms > 0 && !v.watched) { "Resume" } else { "Play" }
                                    </a>
                                }
                                if let Some(viewing) = &timeline.viewing {
                                    if viewing.watched {
                                        <p class="viewing is-watched">icon(name: "check") "Watched."</p>
                                    } else if viewing.position_ms > 0 {
                                        <p class="viewing">"Stopped at " (clock(viewing.position_ms)) "."</p>
                                        if let Some(duration) = timeline.duration_ms {
                                            <progress class="viewing-progress" max=(duration) value=(viewing.position_ms)
                                                aria-label=(format!("{}% watched", viewing.position_ms * 100 / duration.max(1)))></progress>
                                        }
                                    } else {
                                        <p class="viewing">"Not started."</p>
                                    }
                                } else {
                                    <p class="viewing muted">"Viewing history is unavailable."</p>
                                }
                            </div>
                            <h3 class="versions-title">"Versions"</h3>
                            <ul class="versions">
                                for version in timeline.versions.iter() {
                                    <li class="version">
                                        <span class="version-label">(version.label.as_str())</span>
                                        <span class=(if version.availability == Availability::Available { "availability is-ok" } else { "availability" })>(availability_text(version.availability))</span>
                                        <span class="version-summary">(version.summary.as_str())</span>
                                    </li>
                                }
                            </ul>
                        </section>
                    }
                </div>
            </div>
            if !view_model.children.is_empty() {
                <section class="shelf" aria-labelledby="entries-heading">
                    <h2 class="shelf-title" id="entries-heading">
                        (entries_heading)
                    </h2>
                    item_grid(items: view_model.children, label: "Entries")
                </section>
            }
        </article>
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
        <form role="search" class="search page-search" method="get" action=(href!(search))>
            <label class="visually-hidden" for="page-search">"Search the catalog"</label>
            icon(name: "search")
            <input id="page-search" type="search" name="q" placeholder="Search titles" autocomplete="off"
                enterkeyhint="search">
        </form>
        <header class="page-header">
            <h1 class="page-title">(heading.as_str())</h1>
            if !view_model.query.is_empty() && !view_model.items.is_empty() {
                <p class="page-count">(view_model.items.len()) if view_model.items.len() == 1 { " title" } else { " titles" }</p>
            }
        </header>
        if view_model.query.is_empty() {
            empty(message: "Search by title.")
        } else if view_model.items.is_empty() {
            empty(message: "No titles match.")
        } else {
            item_grid(items: view_model.items, label: "Search results")
        }
    })
}

#[page("/libraries")]
async fn library_index(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let view_model = facade(cx).0.home(who).await.map_err(ui_error)?;
    Ok(view! {
        <h1 class="page-title">"Libraries"</h1>
        if view_model.libraries.is_empty() {
            empty(message: "No libraries yet.")
        } else {
            <ul class="list-rows" aria-label="Libraries">
                for lib in view_model.libraries {
                    <li>
                        <a class="list-row" href=(href!(library, LibraryId(lib.id.as_str())))>
                            <span class=(format!("list-art {}", art_tone(&lib.id))) aria-hidden="true">
                                icon(name: library_icon(&lib.kind))
                            </span>
                            <span class="list-text">
                                <span class="card-title">(lib.name.as_str())</span>
                                <span class="card-meta">(kind_label(&lib.kind))
                                    <span class="meta-sep" aria-hidden="true"></span>
                                    (availability_text(lib.availability))
                                </span>
                            </span>
                            icon(name: "chevron")
                        </a>
                    </li>
                }
            </ul>
        }
    })
}

/// The administrative screens as one list: the phone tab bar's Manage tab.
#[page("/manage")]
async fn manage(cx: &Cx) -> Result<impl View> {
    let links = manage_links(principal(cx)?);
    if links.is_empty() {
        return Err(forbidden().into());
    }
    Ok(view! {
        <h1 class="page-title">"Manage"</h1>
        <ul class="list-rows" aria-label="Manage">
            for link in links {
                <li>
                    <a class="list-row" href=(link.href)>
                        <span class="list-art" aria-hidden="true">icon(name: link.icon_name)</span>
                        <span class="list-text"><span class="card-title">(link.label)</span></span>
                        icon(name: "chevron")
                    </a>
                </li>
            }
        </ul>
    })
}

/// Emits a stable player host. The external playback module owns everything
/// inside `#motion-player`; this page is never partially re-rendered.
#[page("/play/{timeline_id}")]
async fn play(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let id = path_param::<TimelineId>(cx);
    let view_model = facade(cx).0.player(who, id).await.map_err(ui_error)?;
    let preferences = facade(cx)
        .0
        .profiles(who)
        .await
        .map_err(ui_error)?
        .preferences;
    let versions = facade(cx)
        .0
        .item(who, &view_model.item_id)
        .await
        .map_err(ui_error)?
        .timelines
        .into_iter()
        .find(|timeline| timeline.id == view_model.timeline_id)
        .map(|timeline| timeline.versions)
        .unwrap_or_default();
    Ok(view! {
        <div class="player-page">
        <header class="player-header">
            <a class="player-back" href=(href!(item, ItemId(view_model.item_id.as_str())))>icon(name: "back") <span class="player-back-label">"Back to title"</span></a>
            <h1 class="player-title">(view_model.title.as_str())</h1>
        </header>
        <div id="motion-player" class="player-host"
            data-motion-player="v1"
            data-timeline-id=(view_model.timeline_id.as_str())
            data-profile-id=(who.profile_id.as_str())
            data-principal-id=(who.principal_id.as_str())
            data-server-epoch=(who.server_epoch.as_str())
            data-viewing-epoch=(who.viewing_epoch.as_str())
            data-can-save-viewing=(if who.can("viewing:write") { "true" } else { "false" })
            data-viewing-revision=(view_model.viewing_revision.as_str())
            data-viewing-manual-epoch=(view_model.viewing_manual_epoch.as_str())
            data-quality-mode=(preferences.quality_mode.as_str())
            data-autoplay=(if preferences.autoplay { "true" } else { "false" })
            data-subtitle-policy=(preferences.subtitle_mode.as_str())
            data-software-decode=(if preferences.allow_client_software_decode { "true" } else { "false" })
            data-resume-ms=(view_model.resume_ms)
            data-duration-ms=(view_model.duration_ms.unwrap_or(0))
            data-demuxe-base=(assets::DEMUXE_BASE)>
            <p class="status-panel" role="status" data-state="loading">"Preparing playback…"</p>
        </div>
        <form id="motion-playback-controls" class="player-controls" aria-label="Playback controls">
            <div class="player-actions">
                <button type="button" data-player-action="play">icon(name: "play") <span>"Play"</span></button>
                <button type="button" data-player-action="pause">icon(name: "pause") <span>"Pause"</span></button>
                <button type="button" data-player-action="mute">icon(name: "volume") <span>"Mute / unmute"</span></button>
                <button type="button" data-player-action="next">icon(name: "next") <span>"Next title"</span></button>
                <output id="motion-position" aria-live="off">(clock(view_model.resume_ms))</output>
            </div>
            <details class="player-options">
                <summary>"Playback options"</summary>
                <div class="player-options-body">
                    <div class="player-option-row">
                        <label>"Position in seconds "<input name="position" type="number" inputmode="decimal" min="0" step="0.1" required=(true) value=(view_model.resume_ms / 1000)></label>
                        <button type="submit">"Seek"</button>
                    </div>
                    <label>"Version "<select name="version"><option value="">"Automatic"</option>
                        for version in versions {
                            <option value=(version.id.as_str()) disabled=(version.availability != Availability::Available)>(version.label.as_str())</option>
                        }
                    </select></label>
                    <label>"Audio "<select name="audio" data-version=(view_model.audio_version.as_deref().unwrap_or(""))
                        data-file-id=(view_model.audio_file.as_ref().map(|f| f.0.as_str()).unwrap_or(""))
                        data-file-revision=(view_model.audio_file.as_ref().map(|f| f.1.as_str()).unwrap_or(""))><option value="">"Default"</option>
                        for track in view_model.audio_tracks.iter() {
                            <option value=(track.id.as_str())>(track.label.as_str())</option>
                        }
                    </select></label>
                    <label>"Quality "<select name="quality"><option value="auto" selected=(preferences.quality_mode == "auto")>"Automatic"</option><option value="original" selected=(preferences.quality_mode == "original")>"Original"</option><option value="convert" selected=(preferences.quality_mode == "convert")>"Convert"</option></select></label>
                    <button type="button" data-player-action="quality">"Apply quality"</button>
                </div>
            </details>
        </form>
        <script type="module" src=(assets::PLAYER.url())></script>
        <p id="motion-progress-status" role="status"></p>
        </div>
    })
}
