//! Administrative and preference screens (plan 14.2).
//!
//! Reads come from `UiQueryFacade`. Every change is a *command form*: plain
//! HTML whose `data-command` names an existing public `/api/v2` operation.
//! The external bridge module submits it with the session CSRF token, a
//! stable idempotency key and the rendered `If-Match` validator, then
//! reloads the server-rendered page. No Topcoat procedure or route mutates
//! anything (plan 11.7: no mutation-only procedures are authorized).

use topcoat::{
    Result,
    context::Cx,
    router::page,
    view::{Child, View, component, view},
};

use crate::app::{availability_text, empty, facade, principal, ui_error};

/// How the bridge encodes one form field into the JSON body.
#[derive(Clone, Copy)]
enum Field {
    /// Text, sent as a JSON string.
    Text,
    /// Comma-separated text, sent as a JSON array of trimmed strings.
    List,
    /// Checkbox, sent as a JSON boolean.
    Bool,
    /// Number input, sent as a JSON number.
    Number,
    /// Fixed JSON value carried in the input's value.
    Json,
    /// Checkbox contributing its value to a JSON array named by the input.
    Member,
}

impl Field {
    fn attr(self) -> &'static str {
        match self {
            Field::Text => "text",
            Field::List => "list",
            Field::Bool => "bool",
            Field::Number => "number",
            Field::Json => "json",
            Field::Member => "member",
        }
    }
}

/// A form the bridge turns into one public API call.
///
/// `command` is `"<METHOD> <path>"`; `idempotent` adds a stable
/// Idempotency-Key per distinct body until an outcome is known; `if_match`
/// carries the validator of the exact state the user saw.
#[component]
pub(crate) async fn command_form(
    command: String,
    label: &str,
    #[default] idempotent: bool,
    #[default] if_match: Option<String>,
    #[default] class: Option<&'static str>,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    Ok(view! {
        <form class=(class.unwrap_or("command")) data-command=(command.as_str())
            data-idempotent=(if idempotent { "true" } else { "false" })
            data-if-match=(if_match.as_deref())>
            (child)
            <button type="submit">(label)</button>
            <p class="command-status" role="status" aria-live="polite"></p>
        </form>
    })
}

fn kind(field: Field) -> &'static str {
    field.attr()
}

/// Browser pairing offered on the sign-in page. The bridge creates the
/// pairing, shows the user code, polls the claim and exchanges the credential
/// for a session. Device codes never appear in HTML.
#[component]
pub(crate) async fn pairing_form() -> Result<impl View> {
    Ok(view! {
        command_form(command: "POST /api/v2/auth/pairings".to_string(), label: "Pair this browser", class: Some("command pairing"),
            <input type="hidden" name="device_name" data-type=(kind(Field::Text)) value="Web browser">
            <input type="hidden" name="client_name" data-type=(kind(Field::Text)) value="Motion web">
        )
        <p class="muted small">"Pairing creates a revocable credential for this browser after an administrator approves the code."</p>
    })
}

// --- Profiles and preferences ---------------------------------------------

#[page("/profiles")]
pub(crate) async fn profiles(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let model = facade(cx).0.profiles(who).await.map_err(ui_error)?;
    let prefs = model.preferences;
    let path = format!("PUT /api/v2/profiles/{}/preferences", model.current);
    let subtitle_modes = [
        ("off", "Off"),
        ("forced", "Forced only"),
        ("always", "Always"),
        ("foreign_audio", "When audio is in another language"),
    ];
    let quality_modes = [
        ("auto", "Automatic"),
        ("original", "Original only"),
        ("convert", "Prefer converted"),
    ];
    Ok(view! {
        <h1>"Profiles and preferences"</h1>
        <p class="muted">"Profiles separate viewing history and preferences. They are not a security boundary unless the server enforces profile restrictions for this device."</p>
        <h2>"Playback preferences"</h2>
        command_form(command: path, label: "Save preferences", if_match: Some(prefs.etag.clone()), class: Some("command preferences"),
            <label>"Audio languages "
                <input name="audio_languages" data-type=(kind(Field::List)) value=(prefs.audio_languages.join(", ")) placeholder="en, ja">
            </label>
            <label>"Subtitle languages "
                <input name="subtitle_languages" data-type=(kind(Field::List)) value=(prefs.subtitle_languages.join(", ")) placeholder="en">
            </label>
            <label>"Subtitles "
                <select name="subtitle_mode" data-type=(kind(Field::Text))>
                    for (value, text) in subtitle_modes {
                        <option value=(value) selected=(prefs.subtitle_mode == value)>(text)</option>
                    }
                </select>
            </label>
            <label>"Quality "
                <select name="quality_mode" data-type=(kind(Field::Text))>
                    for (value, text) in quality_modes {
                        <option value=(value) selected=(prefs.quality_mode == value)>(text)</option>
                    }
                </select>
            </label>
            <label><input type="checkbox" name="allow_client_software_decode" data-type=(kind(Field::Bool)) checked=(prefs.allow_client_software_decode)>
                " Allow decoding in this app when the browser cannot"</label>
            <label><input type="checkbox" name="autoplay" data-type=(kind(Field::Bool)) checked=(prefs.autoplay)>
                " Play the next episode automatically"</label>
            <label>"Count as watched at "
                <input type="number" name="completion_percent" data-type=(kind(Field::Number)) required=(true) min="50" max="100" step="any" value=(prefs.completion_percent)>"%"
            </label>
        )
    })
}

// --- Sources, libraries and scans -----------------------------------------

fn scan_text(status: &str) -> &'static str {
    match status {
        "queued" => "Waiting to start",
        "running" => "Scanning",
        "complete" => "Complete",
        "cancelled" => "Cancelled",
        "failed" => "Failed",
        "partial" => "Partial — some folders could not be read; nothing unseen was removed",
        "unavailable" => "Source unavailable — nothing was removed",
        "stale" => "Superseded by a newer scan",
        _ => "Unknown",
    }
}

#[page("/sources")]
pub(crate) async fn sources(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let model = facade(cx).0.sources(who).await.map_err(ui_error)?;
    let source_options: Vec<(String, String)> = model
        .sources
        .iter()
        .map(|s| (s.id.clone(), s.name.clone()))
        .collect();
    Ok(view! {
        <h1>"Sources and scans"</h1>
        <section aria-labelledby="sources-heading">
            <h2 id="sources-heading">"Sources"</h2>
            if model.sources.is_empty() {
                empty(message: "No sources yet. Add a folder on the server to start.")
            } else {
                <ul class="rows">
                    for source in model.sources {
                        <li><strong>(source.name.as_str())</strong> " · " (availability_text(source.availability))
                            <div class="muted small"><code>(source.root_path.as_str())</code></div></li>
                    }
                </ul>
            }
            <h3>"Add a source"</h3>
            command_form(command: "POST /api/v2/sources".to_string(), label: "Add source", idempotent: true, class: Some("command inline"),
                <label>"Name " <input name="name" data-type=(kind(Field::Text)) required=(true) maxlength="200"></label>
                <label>"Folder on the server " <input name="root_path" data-type=(kind(Field::Text)) required=(true) maxlength="4096" placeholder="/srv/media/movies"></label>
                <input type="hidden" name="exclusions" data-type=(kind(Field::Json)) value="[]">
            )
            <p class="muted small">"Paths are on the server’s file system, not on the device you are browsing from."</p>
        </section>
        <section aria-labelledby="libraries-heading">
            <h2 id="libraries-heading">"Libraries"</h2>
            <ul class="rows">
                for library in model.libraries {
                    <li>
                        <strong>(library.name.as_str())</strong> " · " (availability_text(library.availability))
                        command_form(command: format!("POST /api/v2/libraries/{}/scans", library.id), label: "Scan now", idempotent: true, class: Some("command inline"),
                            <label>"Mode "
                                <select name="mode" data-type=(kind(Field::Text))>
                                    <option value="incremental">"Changes only"</option>
                                    <option value="verify">"Verify file contents"</option>
                                </select>
                            </label>
                            <input type="hidden" name="require_complete" data-type=(kind(Field::Json)) value="false">
                        )
                    </li>
                }
            </ul>
            <h3>"Add a library"</h3>
            command_form(command: "POST /api/v2/libraries".to_string(), label: "Add library", idempotent: true, class: Some("command inline"),
                <label>"Name " <input name="name" data-type=(kind(Field::Text)) required=(true) maxlength="200"></label>
                <label>"Kind "
                    <select name="kind" data-type=(kind(Field::Text))>
                        <option value="movies">"Movies"</option>
                        <option value="television">"Television"</option>
                        <option value="personal_video">"Personal video"</option>
                        <option value="mixed">"Mixed"</option>
                    </select>
                </label>
                <input type="hidden" name="language" data-type=(kind(Field::Text)) value="en">
                // Always present (possibly empty) as LibraryInput requires; checked sources are appended.
                <input type="hidden" name="source_ids" data-type=(kind(Field::Json)) value="[]">
                <fieldset><legend>"Sources"</legend>
                    for (id, name) in source_options {
                        <label><input type="checkbox" name="source_ids" value=(id.as_str()) data-type=(kind(Field::Member))>" " (name.as_str())</label>
                    }
                </fieldset>
            )
        </section>
        <section aria-labelledby="scans-heading">
            <h2 id="scans-heading">"Recent scans"</h2>
            if model.scans.is_empty() {
                empty(message: "No scans yet.")
            } else {
                <ul class="rows">
                    for scan in model.scans {
                        <li data-scan-status=(scan.status.as_str())>
                            <strong>(scan.library_name.as_str())</strong> " — " (scan_text(&scan.status))
                            <span class="muted small">" · started " (scan.started.as_str())</span>
                            <ul class="small">
                                for source in scan.sources {
                                    <li>(source.source_name.as_str()) ": " (scan_text(&source.status)) ", "
                                        (source.observed_files) " files seen, " (source.complete_directories) " folders complete"
                                        if source.incomplete_directories > 0 {
                                            ", " <strong>(source.incomplete_directories) " incomplete"</strong>
                                        }
                                        if !source.error_codes.is_empty() { " · " (source.error_codes.join(", ")) }
                                    </li>
                                }
                            </ul>
                            if scan.status == "queued" || scan.status == "running" {
                                command_form(command: format!("POST /api/v2/scans/{}/cancel", scan.id), label: "Cancel scan", idempotent: true, class: Some("command inline"))
                            }
                        </li>
                    }
                </ul>
            }
        </section>
    })
}

// --- Matches and corrections ----------------------------------------------

#[page("/matches")]
pub(crate) async fn matches(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let rows = facade(cx).0.matches(who).await.map_err(ui_error)?;
    Ok(view! {
        <h1>"Matches and corrections"</h1>
        <p class="muted">"Decisions are checked against the exact file and proposal revisions shown here. If either changed, you will be asked to review again."</p>
        if rows.is_empty() {
            empty(message: "Nothing needs review.")
        } else {
            for row in rows {
                <section class="match" aria-labelledby=(format!("match-{}", row.id))>
                    <h2 id=(format!("match-{}", row.id))>(row.subject.as_str()) <span class="muted small">" · " (row.status.as_str())</span></h2>
                    if (row.status == "review" || row.status == "pending" || row.status == "deferred") && row.candidates.is_empty() {
                        <p class="muted">"No candidates were proposed."</p>
                    }
                    if (row.status == "review" || row.status == "pending" || row.status == "deferred") && !row.candidates.is_empty() {
                        command_form(command: format!("PUT /api/v2/catalog/matches/{}/decision", row.id), label: "Accept selected", if_match: Some(row.etag.clone()),
                            <input type="hidden" name="decision" data-type=(kind(Field::Text)) value="accept">
                            <fieldset><legend>"Candidates"</legend>
                                for (index, candidate) in row.candidates.iter().enumerate() {
                                    <label class="candidate">
                                        <input type="radio" name="candidate_id" data-type=(kind(Field::Text)) value=(candidate.id.as_str()) required=(true) checked=(index == 0)>
                                        " " (candidate.title.as_str())
                                        if let Some(confidence) = candidate.confidence_percent { <span class="muted">" · " (confidence) "% confidence"</span> }
                                        if !candidate.reasons.is_empty() { <span class="muted">" · " (candidate.reasons.join(", "))</span> }
                                    </label>
                                }
                            </fieldset>
                        )
                    }
                    if row.status == "review" || row.status == "pending" || row.status == "deferred" {
                        command_form(command: format!("PUT /api/v2/catalog/matches/{}/decision", row.id), label: "None of these", if_match: Some(row.etag.clone()), class: Some("command inline"),
                            <input type="hidden" name="decision" data-type=(kind(Field::Text)) value="reject">
                            <input type="hidden" name="candidate_id" data-type=(kind(Field::Json)) value="null">
                        )
                        command_form(command: format!("PUT /api/v2/catalog/matches/{}/decision", row.id), label: "Decide later", if_match: Some(row.etag.clone()), class: Some("command inline"),
                            <input type="hidden" name="decision" data-type=(kind(Field::Text)) value="defer">
                            <input type="hidden" name="candidate_id" data-type=(kind(Field::Json)) value="null">
                        )
                    }
                </section>
            }
        }
    })
}

// --- Processing --------------------------------------------------------------

#[page("/processing")]
pub(crate) async fn processing(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let jobs = facade(cx).0.jobs(who).await.map_err(ui_error)?;
    Ok(view! {
        <h1>"Processing"</h1>
        if jobs.is_empty() {
            empty(message: "No background work.")
        } else {
            <table>
                <caption class="visually-hidden">"Background jobs"</caption>
                <thead><tr><th scope="col">"Job"</th><th scope="col">"State"</th><th scope="col">"Progress"</th><th scope="col"><span class="visually-hidden">"Actions"</span></th></tr></thead>
                <tbody>
                    for job in jobs {
                        <tr>
                            <td>(job.kind.as_str()) " " <span class="muted small">(job.id.as_str())</span></td>
                            <td>
                                if job.phase == "cancelling" { "Cancelling (not yet confirmed)" } else { (job.phase.as_str()) }
                                if let Some(code) = &job.error_code { " · " (code.as_str()) }
                            </td>
                            <td>
                                if let Some(percent) = job.progress_percent {
                                    <progress max="100" value=(percent) aria-label=(format!("{percent}%"))></progress>
                                } else { "—" }
                            </td>
                            <td>
                                if !matches!(job.phase.as_str(), "completed" | "failed" | "cancelled" | "interrupted" | "cancelling") {
                                    command_form(command: format!("POST /api/v2/jobs/{}/cancel", job.id), label: "Cancel", idempotent: true, class: Some("command inline"))
                                }
                            </td>
                        </tr>
                    }
                </tbody>
            </table>
        }
    })
}

// --- Diagnostics -------------------------------------------------------------

#[page("/diagnostics")]
pub(crate) async fn diagnostics(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let d = facade(cx).0.diagnostics(who).await.map_err(ui_error)?;
    let errors = if d.worker_errors.is_empty() {
        "none".to_string()
    } else {
        d.worker_errors.join("; ")
    };
    Ok(view! {
        <h1>"Diagnostics"</h1>
        <dl class="facts">
            <dt>"Server"</dt><dd>(d.server_id.as_str()) " (" (d.server_version.as_str()) ")"</dd>
            <dt>"API"</dt><dd>(d.api_version.as_str()) ", schema " (d.schema_version.as_str())</dd>
            <dt>"Health"</dt><dd>(d.health.as_str())</dd>
            <dt>"Uptime"</dt><dd>(d.uptime_seconds) "s"</dd>
            <dt>"Active deliveries"</dt><dd>(d.active_deliveries)</dd>
            <dt>"Jobs"</dt><dd>(d.running_jobs) " running, " (d.queued_jobs) " queued"</dd>
            <dt>"Worker errors"</dt><dd>(errors.as_str())</dd>
        </dl>
    })
}
