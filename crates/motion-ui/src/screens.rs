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
    router::{page, path_param},
    view::{Child, View, component, view},
};

use crate::{
    app::{availability_text, empty, facade, principal, ui_error},
    facade::{Availability, FilterTermView, MembershipRow},
};

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
    /// Number of minutes, sent as whole seconds under `data-target`.
    Minutes,
    /// Text omitted from the body when blank.
    Optional,
    /// Integer omitted from the body when blank.
    OptionalInt,
    /// Text sent as JSON null when blank.
    Nullable,
    /// One typed filter term appended to the array named by the input, only
    /// when a value is chosen (`data-field`, `data-operator`, `data-value-type`).
    Term,
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
            Field::Minutes => "minutes",
            Field::Optional => "optional",
            Field::OptionalInt => "optional-int",
            Field::Nullable => "nullable",
            Field::Term => "term",
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
    #[default] file_body: bool,
    #[default] json_body: Option<String>,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    let body_kind = if file_body {
        Some("file")
    } else if json_body.is_some() {
        Some("json")
    } else {
        None
    };
    Ok(view! {
        <form class=(class.unwrap_or("command")) data-command=(command.as_str())
            data-idempotent=(if idempotent { "true" } else { "false" })
            data-if-match=(if_match.as_deref())
            data-body=(body_kind)>
            if let Some(body) = &json_body {
                <input type="hidden" data-json-body="" value=(body.as_str())>
            }
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

fn task_text(task: &str) -> &'static str {
    match task {
        "scan" => "Scan for changes",
        "metadata_refresh" => "Refresh local metadata",
        "preview" => "Generate previews",
        "optimize" => "Prepare versions",
        _ => "Unknown task",
    }
}

/// A whole number of hours when exact, else minutes; schedules are at least a minute.
fn interval_text(seconds: u64) -> String {
    match seconds {
        s if s % 86_400 == 0 => format!("every {} day(s)", s / 86_400),
        s if s % 3_600 == 0 => format!("every {} hour(s)", s / 3_600),
        s if s % 60 == 0 => format!("every {} minute(s)", s / 60),
        s => format!("every {s} second(s)"),
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
    let library_options: Vec<(String, String)> = model
        .libraries
        .iter()
        .map(|l| (l.library.id.clone(), l.library.name.clone()))
        .collect();
    // The libraries each source belongs to: a source is removable only once
    // no library uses it (the server refuses otherwise).
    let used_by = |source: &str| -> Vec<String> {
        model
            .libraries
            .iter()
            .filter(|l| l.source_ids.iter().any(|id| id == source))
            .map(|l| l.library.name.clone())
            .collect()
    };
    let source_users: Vec<Vec<String>> = model.sources.iter().map(|s| used_by(&s.id)).collect();
    let kinds = [
        ("movies", "Movies"),
        ("television", "Television"),
        ("personal_video", "Personal video"),
        ("mixed", "Mixed"),
    ];
    let tasks = [
        ("scan", "Scan for changes"),
        ("metadata_refresh", "Refresh local metadata"),
    ];
    Ok(view! {
        <h1>"Sources and scans"</h1>
        <section aria-labelledby="sources-heading">
            <h2 id="sources-heading">"Sources"</h2>
            if model.sources.is_empty() {
                empty(message: "No sources yet. Add a folder on the server to start.")
            } else {
                <ul class="rows">
                    for (source, users) in model.sources.into_iter().zip(source_users) {
                        <li data-source=(source.id.as_str())><strong>(source.name.as_str())</strong> " · " (availability_text(source.availability))
                            <div class="muted small"><code>(source.root_path.as_str())</code></div>
                            <details class="disclosure">
                                <summary>"Moved this folder?"</summary>
                                <p class="muted small">"Point the source at its new location. Motion first checks the new folder and shows what will change; files keep their identity, history and corrections only if they are found unchanged."</p>
                                command_form(command: format!("POST /api/v2/sources/{}/relocation-plans", source.id), label: "Check new location", idempotent: true, if_match: Some(source.etag.clone()), class: Some("command inline plan"),
                                    <input type="hidden" data-plan-commit=(format!("POST /api/v2/sources/{}/relocation", source.id))>
                                    <label>"New folder on the server " <input name="new_root_path" data-type=(kind(Field::Text)) required=(true) maxlength="4096" value=(source.root_path.as_str())></label>
                                )
                            </details>
                            <details class="disclosure">
                                <summary>"Remove this source…"</summary>
                                if users.is_empty() {
                                    <p class="muted small">"Nothing is deleted from the folder. If titles were found in it, the source is kept but disabled and its files are marked unavailable, keeping their history and corrections; otherwise its registration is removed. Not possible while a scan or preparation uses it."</p>
                                    command_form(command: format!("DELETE /api/v2/sources/{}", source.id), label: "Remove source", if_match: Some(source.etag.clone()), class: Some("command inline"),
                                        <label><input type="checkbox" required=(true)> " I understand what removing this source does"</label>
                                    )
                                } else {
                                    <p class="muted small">(format!("Used by {}. Remove it from those libraries first (Edit library).", users.join(", ")))</p>
                                }
                            </details>
                        </li>
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
                for row in model.libraries {
                    <li data-library=(row.library.id.as_str())>
                        <strong>(row.library.name.as_str())</strong> " · " (availability_text(row.library.availability))
                        // Starting a scan does not depend on reading the history.
                        command_form(command: format!("POST /api/v2/libraries/{}/scans", row.library.id), label: "Scan now", idempotent: true, class: Some("command inline"),
                            <label>"Mode "
                                <select name="mode" data-type=(kind(Field::Text))>
                                    <option value="incremental">"Changes only"</option>
                                    <option value="verify">"Verify file contents"</option>
                                </select>
                            </label>
                            <input type="hidden" name="require_complete" data-type=(kind(Field::Json)) value="false">
                        )
                        <details class="disclosure">
                            <summary>"Edit library"</summary>
                            command_form(command: format!("PUT /api/v2/libraries/{}", row.library.id), label: "Save library", if_match: Some(row.etag.clone()), class: Some("command"),
                                <label>"Name " <input name="name" data-type=(kind(Field::Text)) required=(true) maxlength="200" value=(row.library.name.as_str())></label>
                                <label>"Kind "
                                    <select name="kind" data-type=(kind(Field::Text))>
                                        for (value, text) in kinds {
                                            <option value=(value) selected=(row.library.kind == value)>(text)</option>
                                        }
                                    </select>
                                </label>
                                <label>"Language " <input name="language" data-type=(kind(Field::Text)) maxlength="35" value=(row.language.as_str())></label>
                                <input type="hidden" name="source_ids" data-type=(kind(Field::Json)) value="[]">
                                <fieldset><legend>"Sources"</legend>
                                    for (id, name) in source_options.iter() {
                                        <label><input type="checkbox" name="source_ids" value=(id.as_str()) data-type=(kind(Field::Member)) checked=(row.source_ids.contains(id))>" " (name.as_str())</label>
                                    }
                                </fieldset>
                            )
                        </details>
                        <details class="disclosure">
                            <summary>"Delete this library…"</summary>
                            <p class="muted small">"Removes only the library and its finished scan history. Its sources, files, titles, corrections, collections and viewing history stay, and nothing is deleted from disk. Not possible while one of its scans is pending."</p>
                            command_form(command: format!("DELETE /api/v2/libraries/{}", row.library.id), label: "Delete library", if_match: Some(row.etag.clone()), class: Some("command inline"),
                                <label><input type="checkbox" required=(true)> " I understand what deleting this library does"</label>
                            )
                        </details>
                    </li>
                }
            </ul>
            <h3>"Add a library"</h3>
            command_form(command: "POST /api/v2/libraries".to_string(), label: "Add library", idempotent: true, class: Some("command inline"),
                <label>"Name " <input name="name" data-type=(kind(Field::Text)) required=(true) maxlength="200"></label>
                <label>"Kind "
                    <select name="kind" data-type=(kind(Field::Text))>
                        for (value, text) in kinds {
                            <option value=(value)>(text)</option>
                        }
                    </select>
                </label>
                <input type="hidden" name="language" data-type=(kind(Field::Text)) value="en">
                // Always present (possibly empty) as LibraryInput requires; checked sources are appended.
                <input type="hidden" name="source_ids" data-type=(kind(Field::Json)) value="[]">
                <fieldset><legend>"Sources"</legend>
                    for (id, name) in source_options.iter() {
                        <label><input type="checkbox" name="source_ids" value=(id.as_str()) data-type=(kind(Field::Member))>" " (name.as_str())</label>
                    }
                </fieldset>
            )
        </section>
        <section aria-labelledby="scans-heading">
            <h2 id="scans-heading">"Recent scans"</h2>
            if !model.scans_available {
                empty(message: "Scan history cannot be shown right now.")
            } else if model.scans.is_empty() {
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
        <section aria-labelledby="schedules-heading">
            <h2 id="schedules-heading">"Schedules"</h2>
            if let Some(schedules) = model.schedules {
                if schedules.is_empty() {
                    empty(message: "Nothing runs on a schedule.")
                } else {
                    <ul class="rows">
                        for schedule in schedules {
                            <li data-schedule=(schedule.id.as_str())>
                                <strong>(schedule.library_name.as_str())</strong> " — " (task_text(&schedule.task)) ", " (interval_text(schedule.interval_seconds))
                                if !schedule.enabled { <span class="muted">" · paused"</span> }
                                command_form(command: format!("PUT /api/v2/schedules/{}", schedule.id), label: "Save", if_match: Some(schedule.etag.clone()), class: Some("command inline"),
                                    <input type="hidden" name="library_id" data-type=(kind(Field::Text)) value=(schedule.library_id.as_str())>
                                    <input type="hidden" name="task" data-type=(kind(Field::Text)) value=(schedule.task.as_str())>
                                    // Intervals set through the API need not be whole minutes; never round them.
                                    if schedule.interval_seconds % 60 == 0 {
                                        <label>"Every (minutes) " <input type="number" name="interval_minutes" data-type=(kind(Field::Minutes)) data-target="interval_seconds" required=(true) min="1" max="525600" step="1" value=(schedule.interval_seconds / 60)></label>
                                    } else {
                                        <label>"Every (seconds) " <input type="number" name="interval_seconds" data-type=(kind(Field::Number)) required=(true) min="60" max="31536000" step="1" value=(schedule.interval_seconds)></label>
                                    }
                                    <label><input type="checkbox" name="enabled" data-type=(kind(Field::Bool)) checked=(schedule.enabled)>" Enabled"</label>
                                )
                                command_form(command: format!("DELETE /api/v2/schedules/{}", schedule.id), label: "Remove schedule", if_match: Some(schedule.etag.clone()), class: Some("command inline"))
                            </li>
                        }
                    </ul>
                }
                if !library_options.is_empty() {
                    <h3>"Add a schedule"</h3>
                    command_form(command: "POST /api/v2/schedules".to_string(), label: "Add schedule", idempotent: true, class: Some("command inline"),
                        <label>"Library "
                            <select name="library_id" data-type=(kind(Field::Text))>
                                for (id, name) in library_options {
                                    <option value=(id.as_str())>(name.as_str())</option>
                                }
                            </select>
                        </label>
                        <label>"Task "
                            <select name="task" data-type=(kind(Field::Text))>
                                for (value, text) in tasks {
                                    <option value=(value)>(text)</option>
                                }
                            </select>
                        </label>
                        <label>"Every (minutes) " <input type="number" name="interval_minutes" data-type=(kind(Field::Minutes)) data-target="interval_seconds" required=(true) min="1" max="525600" step="1" value="1440"></label>
                        <input type="hidden" name="enabled" data-type=(kind(Field::Json)) value="true">
                    )
                }
            } else {
                empty(message: "Schedules are unavailable on this server.")
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
        <p class="muted">"Prepare a version of a title from its management page. Failed or cancelled work can be retried here."</p>
        if jobs.is_empty() {
            empty(message: "No background work.")
        } else {
            <div class="table-scroll" role="region" aria-label="Background jobs" tabindex="0"><table>
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
                                // The server retries failed and cancelled jobs only.
                                if matches!(job.phase.as_str(), "failed" | "cancelled") {
                                    command_form(command: format!("POST /api/v2/jobs/{}/retry", job.id), label: "Retry", idempotent: true, class: Some("command inline"))
                                }
                            </td>
                        </tr>
                    }
                </tbody>
            </table></div>
        }
    })
}

// --- Downloads ---------------------------------------------------------------

fn size_text(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.0} MB", b as f64 / (1u64 << 20) as f64),
        b => format!("{} KB", b.div_ceil(1024)),
    }
}

/// This device's download demands. The desktop host transfers ready ones and
/// removes local copies of cancelled or expired ones when it next connects.
#[page("/downloads")]
pub(crate) async fn downloads(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let rows = facade(cx).0.downloads(who).await.map_err(ui_error)?;
    Ok(view! {
        <h1>"Downloads"</h1>
        <p class="muted">"The Motion desktop app saves ready downloads on this device and keeps offline progress until it is reconciled here. Cancelling removes the device copy the next time the app connects; it cannot reach copies on disconnected devices."</p>
        if rows.is_empty() {
            empty(message: "Nothing is saved for offline viewing.")
        } else {
            <div class="table-scroll" role="region" aria-label="Downloads" tabindex="0"><table>
                <caption class="visually-hidden">"Downloads"</caption>
                <thead><tr><th scope="col">"Title"</th><th scope="col">"State"</th><th scope="col">"Size"</th><th scope="col"><span class="visually-hidden">"Actions"</span></th></tr></thead>
                <tbody>
                    for row in rows {
                        <tr>
                            <td>if row.title.is_empty() { <span class="muted">"Unavailable title"</span> } else { (row.title.as_str()) }</td>
                            <td>(match row.state.as_str() {
                                "ready" => "Ready to save",
                                "expired" => "Expired — the source changed",
                                "cancelled" => "Cancelled",
                                _ => "Unknown",
                            })</td>
                            <td>(size_text(row.size_bytes))</td>
                            <td>
                                if row.state != "cancelled" {
                                    command_form(command: format!("DELETE /api/v2/downloads/{}", row.id), label: "Cancel", class: Some("command inline"))
                                }
                            </td>
                        </tr>
                    }
                </tbody>
            </table></div>
        }
    })
}

/// Request a device-owned download of one timeline's original. Shown only in
/// the desktop host, which performs the transfer; the server decides whether
/// the timeline can be downloaded and explains any refusal.
#[component]
pub(crate) async fn download_form(
    device_id: String,
    profile_id: String,
    timeline_id: String,
) -> Result<impl View> {
    let tracks = r#"{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"auto","audio_track_id":null,"subtitle_track_id":null}"#;
    Ok(view! {
        <div class="download-action" data-desktop-only="true" hidden=(true)>
            command_form(command: "POST /api/v2/downloads".to_string(), label: "Save for offline", idempotent: true, class: Some("command inline"),
                <input type="hidden" name="device_id" data-type=(kind(Field::Text)) value=(device_id)>
                <input type="hidden" name="profile_id" data-type=(kind(Field::Text)) value=(profile_id)>
                <input type="hidden" name="timeline_id" data-type=(kind(Field::Text)) value=(timeline_id)>
                <input type="hidden" name="version_id" data-type=(kind(Field::Json)) value="null">
                <input type="hidden" name="recipe_id" data-type=(kind(Field::Json)) value="null">
                <input type="hidden" name="tracks" data-type=(kind(Field::Json)) value=(tracks)>
            )
        </div>
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

// --- Title management -------------------------------------------------------

path_param!(item_id);

fn match_state_text(state: &str) -> &'static str {
    match state {
        "matched" => "Identified automatically",
        "manual" => "Identified by hand",
        "ambiguous" => "Several possible matches — needs review",
        "unmatched" => "Not identified",
        _ => "Unknown",
    }
}

fn rendition_text(status: &str) -> &'static str {
    match status {
        "pending" => "Preparing",
        "validating" => "Checking the result",
        "ready" => "Ready",
        "stale" => "Out of date — the original changed",
        "failed" => "Failed",
        "expired" => "Expired",
        _ => "Unknown",
    }
}

fn field_label(field: &str) -> &str {
    match field {
        "title" => "Title",
        "description" => "Description",
        "release_year" => "Year",
        "release_date" => "Release date",
        "cast" => "Cast",
        "tags" => "Tags",
        other => other,
    }
}

fn source_label(source: &str) -> &str {
    match source {
        "scan" => "file name",
        "nfo" => ".nfo file",
        "local" => "your edit",
        other => other,
    }
}

fn sources_text(names: &[String]) -> String {
    names
        .iter()
        .map(|s| source_label(s))
        .collect::<Vec<_>>()
        .join(", ")
}

#[component]
async fn membership(
    rows: Vec<MembershipRow>,
    command_base: &'static str,
    add: &'static str,
    remove: &'static str,
) -> Result<impl View> {
    Ok(view! {
        <ul class="rows">
            for row in rows {
                <li>
                    <strong>(row.name.as_str())</strong>
                    if row.contains { <span class="muted">" · includes this title"</span> }
                    command_form(command: format!("PUT {command_base}/{}", row.id), label: if row.contains { remove } else { add }, if_match: Some(row.etag.clone()), json_body: Some(row.toggle_body.clone()), class: Some("command inline"))
                </li>
            }
        </ul>
    })
}

#[page("/item/{item_id}/manage")]
pub(crate) async fn manage_item(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let id = path_param::<ItemId>(cx);
    let model = facade(cx).0.manage_item(who, id).await.map_err(ui_error)?;
    let item_id = model.item.id.clone();
    let back = format!("/item/{item_id}");
    let can_write = who.can("catalog:write");
    let can_prepare = who.can("processing:request");
    let can_organize = who.can("collections:write");
    let recipes = model.recipes.clone().unwrap_or_default();
    Ok(view! {
        <a class="back-link" href=(back.as_str())>"Back to title"</a>
        <h1>"Manage “" (model.item.title.as_str()) "”"</h1>
        <p class="muted">(match_state_text(&model.match_state))
            if model.item.availability != Availability::Available { " · " (availability_text(model.item.availability)) }
        </p>

        if can_write {
            <section aria-labelledby="details-heading">
                <h2 id="details-heading">"Details"</h2>
                if let Some(metadata) = &model.metadata {
                    if !metadata.effective.is_empty() {
                        <dl class="facts">
                            for (field, value) in metadata.effective.iter() {
                                <dt>(field_label(field))</dt>
                                <dd>(value.as_str())
                                    if let Some((_, origins)) = metadata.field_sources.iter().find(|(f, _)| f == field) {
                                        <span class="muted small">" · from " (sources_text(origins))</span>
                                    }
                                </dd>
                            }
                        </dl>
                    }
                    <p class="muted small">"Your edits are kept separately and survive rescans. Values read from file names and .nfo files are refreshed by each scan; a blank field here leaves those values in place."</p>
                    command_form(command: format!("PUT /api/v2/catalog/items/{item_id}/metadata/contributions/local"), label: "Save details", if_match: Some(metadata.etag.clone()), class: Some("command stacked"),
                        <input type="hidden" name="values" data-type=(kind(Field::Json)) value=(metadata.preserved_values.as_str())>
                        <label>"Title " <input name="values.title" data-type=(kind(Field::Optional)) maxlength="500" value=(metadata.title.as_str())></label>
                        <label>"Year " <input type="number" name="values.release_year" data-type=(kind(Field::OptionalInt)) min="1" max="9999" step="1" value=(metadata.release_year.as_str())></label>
                        <label>"Description " <textarea name="values.description" data-type=(kind(Field::Optional)) maxlength="8000" rows="4">(metadata.description.as_str())</textarea></label>
                        <label>"Tags " <input name="tags" data-type=(kind(Field::List)) value=(metadata.tags.join(", ")) placeholder="noir, festival"></label>
                        <label>"Hide tags " <input name="excluded_tags" data-type=(kind(Field::List)) value=(metadata.excluded_tags.join(", "))></label>
                        <input type="hidden" name="locked_fields" data-type=(kind(Field::Json)) value="[]">
                    )
                    command_form(command: format!("POST /api/v2/catalog/items/{item_id}/metadata/refresh"), label: "Re-read .nfo files", idempotent: true, class: Some("command inline"))
                } else {
                    empty(message: "Details are unavailable right now.")
                }
            </section>

            <section aria-labelledby="artwork-heading">
                <h2 id="artwork-heading">"Artwork"</h2>
                if let Some(artwork) = &model.artwork {
                    if artwork.is_empty() {
                        empty(message: "No artwork yet.")
                    } else {
                        <ul class="artwork-grid">
                            for art in artwork.iter() {
                                <li class=(if art.selected { "artwork is-selected" } else { "artwork" })>
                                    <img src=(art.content_url.as_str()) alt="" loading="lazy" decoding="async">
                                    if art.selected {
                                        <p class="small">"Current poster"</p>
                                    }
                                    command_form(command: format!("PUT /api/v2/catalog/items/{item_id}/artwork/selection"), label: "Use", if_match: Some(model.item_etag.clone()), class: Some("command inline"),
                                        <label>"As "
                                            <select name="role" data-type=(kind(Field::Text))>
                                                for (value, text) in [("poster", "Poster"), ("backdrop", "Backdrop"), ("banner", "Banner"), ("thumbnail", "Thumbnail")] {
                                                    <option value=(value) selected=(value == "poster" && !art.selected)>(text)</option>
                                                }
                                            </select>
                                        </label>
                                        <input type="hidden" name="asset_id" data-type=(kind(Field::Text)) value=(art.id.as_str())>
                                    )
                                </li>
                            }
                        </ul>
                    }
                    command_form(command: format!("POST /api/v2/catalog/items/{item_id}/artwork"), label: "Upload", idempotent: true, file_body: true, class: Some("command inline"),
                        <label>"Image " <input type="file" name="file" accept="image/png,image/jpeg,image/webp" required=(true)></label>
                    )
                    <p class="muted small">"PNG, JPEG or WebP, up to 8 MB. Backdrop, banner and thumbnail choices are saved, but only the current poster is shown here. Online artwork providers are not available yet."</p>
                } else {
                    empty(message: "Artwork is unavailable right now.")
                }
            </section>

            <section aria-labelledby="identify-heading">
                <h2 id="identify-heading">"Identification"</h2>
                if let Some(files) = &model.files {
                    if files.is_empty() {
                        empty(message: "This title has no files you can identify.")
                    } else {
                        <p class="muted small">"Look for other titles in this library that this file belongs to. Proposals appear under Matches, where accepting one moves the file to that title. Online providers are not available yet."</p>
                        <ul class="rows">
                            for file in files.iter() {
                                <li>(file.label.as_str()) " · " (availability_text(file.availability))
                                    if file.library_id.is_empty() {
                                        <span class="muted small">" · not in a library you can manage"</span>
                                    } else {
                                        command_form(command: "POST /api/v2/catalog/matches".to_string(), label: "Find matches", idempotent: true, class: Some("command inline"),
                                            <input type="hidden" name="file" data-type=(kind(Field::Json)) value=(serde_json::json!({"file_id": file.file_id, "file_revision": file.file_revision}).to_string())>
                                            <input type="hidden" name="library_id" data-type=(kind(Field::Text)) value=(file.library_id.as_str())>
                                            <input type="hidden" name="provider" data-type=(kind(Field::Json)) value="null">
                                        )
                                    }
                                </li>
                            }
                        </ul>
                        <p><a href="/matches">"Review matches"</a></p>
                    }
                } else {
                    empty(message: "Files are unavailable right now.")
                }
            </section>
        }

        if can_prepare {
            <section aria-labelledby="prepare-heading">
                <h2 id="prepare-heading">"Prepared versions"</h2>
                if let Some(timelines) = &model.timelines {
                    <p class="muted small">"A prepared version is converted once and kept, so it plays without converting live. The chosen audio track is kept exactly; subtitles cannot be included in prepared versions yet."</p>
                    for timeline in timelines.iter() {
                        <section class="edition" aria-label=(format!("Edition {}", timeline.edition))>
                            <h3>(timeline.edition.as_str())</h3>
                            if let Some(renditions) = &timeline.renditions {
                                if renditions.is_empty() {
                                    <p class="muted">"No prepared versions."</p>
                                } else {
                                    <ul class="rows">
                                        for rendition in renditions.iter() {
                                            <li data-rendition-status=(rendition.status.as_str())>(rendition_text(&rendition.status))
                                                <span class="muted small">" · job " (rendition.job_id.as_str())</span></li>
                                        }
                                    </ul>
                                }
                            } else {
                                <p class="muted">"Existing prepared versions cannot be listed right now."</p>
                            }
                            if timeline.sources.is_empty() {
                                <p class="muted">"No original file of this edition can be prepared."</p>
                            }
                            if recipes.is_empty() && !timeline.sources.is_empty() {
                                <p class="muted">"Preparation recipes are unavailable."</p>
                            }
                            if !recipes.is_empty() {
                                for source in timeline.sources.iter() {
                                    command_form(command: "POST /api/v2/processing/preparations".to_string(), label: "Prepare", idempotent: true, class: Some("command inline"),
                                        <input type="hidden" name="source" data-type=(kind(Field::Json)) value=(serde_json::json!({"file_id": source.file_id, "file_revision": source.file_revision}).to_string())>
                                        <input type="hidden" name="timeline_id" data-type=(kind(Field::Text)) value=(timeline.id.as_str())>
                                        <span class="small">(source.label.as_str())</span>
                                        <label>"Recipe "
                                            <select name="recipe_id" data-type=(kind(Field::Text))>
                                                for recipe in recipes.iter() {
                                                    <option value=(recipe.id.as_str())>(recipe.name.as_str()) if recipe.qualification != "qualified" { " (" (recipe.qualification.as_str()) ")" }</option>
                                                }
                                            </select>
                                        </label>
                                        <label>"Audio "
                                            <select name="tracks.audio_track_id" data-type=(kind(Field::Nullable))>
                                                <option value="">"Default track"</option>
                                                for (track, label) in source.audio_tracks.iter() {
                                                    <option value=(track.as_str())>(label.as_str())</option>
                                                }
                                            </select>
                                        </label>
                                        <input type="hidden" name="tracks.audio_component_id" data-type=(kind(Field::Json)) value="null">
                                        <input type="hidden" name="tracks.subtitle_component_id" data-type=(kind(Field::Json)) value="null">
                                        <input type="hidden" name="tracks.subtitle_track_id" data-type=(kind(Field::Json)) value="null">
                                        <input type="hidden" name="tracks.subtitle_policy" data-type=(kind(Field::Text)) value="off">
                                    )
                                }
                            }
                        </section>
                    }
                } else {
                    empty(message: "Preparation is unavailable right now.")
                }
            </section>
        }

        if can_organize {
            <section aria-labelledby="organize-heading">
                <h2 id="organize-heading">"Collections and playlists"</h2>
                if let Some(collections) = model.collections.clone() {
                    if collections.is_empty() {
                        <p class="muted">"No manual collections for this profile. " <a href="/organize">"Create one"</a></p>
                    } else {
                        <h3>"Collections"</h3>
                        membership(rows: collections, command_base: "/api/v2/collections", add: "Add to collection", remove: "Remove from collection")
                    }
                } else {
                    empty(message: "Collections are unavailable right now.")
                }
                if let Some(playlists) = model.playlists.clone() {
                    if !playlists.is_empty() {
                        <h3>"Playlists"</h3>
                        membership(rows: playlists, command_base: "/api/v2/playlists", add: "Add to playlist", remove: "Remove from playlist")
                    }
                } else {
                    empty(message: "Playlists are unavailable right now.")
                }
            </section>
        }

        if can_write {
            <section aria-labelledby="reconcile-heading">
                <h2 id="reconcile-heading">"Duplicates and versions"</h2>
                <p class="muted">"Merging joins another title into this one; splitting moves versions to a new title. Each shows its effect first and changes nothing until you apply it. The old title’s address keeps working and viewing history stays with each timeline."</p>
                if let Some(candidates) = model.merge_candidates.clone() {
                    if candidates.is_empty() {
                        <p class="muted">"No other title with this name was found."</p>
                    } else {
                        <h3>"Merge into this title"</h3>
                        <ul class="rows">
                            for candidate in candidates.iter() {
                                <li><a href=(format!("/item/{}", candidate.id))>(candidate.title.as_str())</a>
                                    command_form(command: format!("POST /api/v2/catalog/items/{item_id}/merge-plans"), label: "Check merge", idempotent: true, class: Some("command inline plan"),
                                        json_body: Some(serde_json::json!({
                                            "source_item_ids": [candidate.id],
                                            "target_item_id": item_id,
                                            "expected_revisions": {candidate.id.clone(): candidate.revision, item_id.clone(): model.item_revision},
                                        }).to_string()),
                                        <input type="hidden" data-plan-commit="POST /api/v2/catalog/reconciliations">
                                    )
                                </li>
                            }
                        </ul>
                    }
                } else {
                    empty(message: "Possible duplicates are unavailable right now.")
                }
                if let Some(versions) = model.versions.clone() {
                    if versions.len() > 1 {
                        <h3>"Split versions into a new title"</h3>
                        command_form(command: format!("POST /api/v2/catalog/items/{item_id}/split-plans"), label: "Check split", idempotent: true, class: Some("command plan"),
                            <fieldset>
                                <legend>"Versions to move"</legend>
                                for version in versions.iter() {
                                    <label><input type="checkbox" name="version_ids" data-type=(kind(Field::Member)) value=(version.id.as_str())> " " (version.label.as_str())</label>
                                }
                            </fieldset>
                            <label>"New title " <input name="new_title" data-type=(kind(Field::Text)) required=(true) maxlength="500"></label>
                            <input type="hidden" name="expected_revision" data-type=(kind(Field::Text)) value=(model.item_revision.as_str())>
                            <input type="hidden" data-plan-commit="POST /api/v2/catalog/reconciliations">
                        )
                    }
                }
            </section>
            <section aria-labelledby="order-heading">
                <h2 id="order-heading">"Play order"</h2>
                <p class="muted">"An explicit order (for example as aired or as on disc) decides what plays next. Placing a timeline keeps its files and viewing history."</p>
                if let Some(ordering) = model.ordering.clone() {
                    if ordering.groups.is_empty() {
                        <p class="muted">"No play orders yet."</p>
                    } else {
                        <ul class="rows">
                            for group in ordering.groups.iter() {
                                <li>(format!("{} ({}) · {}", group.name, group.kind, group.edition))</li>
                            }
                        </ul>
                    }
                    if !ordering.timelines.is_empty() {
                        <h3>"Timelines"</h3>
                        <ul class="rows">
                            for (n, timeline) in ordering.timelines.iter().enumerate() {
                                <li>
                                    (format!("Timeline {} ({})", n + 1, timeline.edition)) " · "
                                    (match (timeline.group_id.as_ref().and_then(|g| ordering.groups.iter().find(|x| &x.id == g)), timeline.position) {
                                        (Some(group), Some(position)) => format!("position {position} in {}", group.name),
                                        _ => "not placed".to_string(),
                                    })
                                    for group in ordering.groups.iter() {
                                        command_form(command: format!("PUT /api/v2/catalog/timelines/{}/order", timeline.id), label: "Place", if_match: Some(timeline.etag.clone()), class: Some("command inline"),
                                            <label>(format!("Position in {} ", group.name))
                                                <input type="number" min="0" max="4294967295" required=(true) name="order_position" data-type=(kind(Field::Number))
                                                    value=(if timeline.group_id.as_deref() == Some(group.id.as_str()) { timeline.position.map(|p| p.to_string()).unwrap_or_default() } else { String::new() })>
                                            </label>
                                            <input type="hidden" name="order_group_id" data-type=(kind(Field::Text)) value=(group.id.as_str())>
                                            <input type="hidden" name="expected_group_revision" data-type=(kind(Field::Text)) value=(group.revision.as_str())>
                                        )
                                    }
                                </li>
                            }
                        </ul>
                    }
                    if !ordering.editions.is_empty() {
                        <h3>"New play order"</h3>
                        command_form(command: format!("POST /api/v2/catalog/items/{item_id}/order-groups"), label: "Create order", idempotent: true, if_match: Some(model.item_etag.clone()), class: Some("command inline"),
                            <label>"Name " <input name="name" data-type=(kind(Field::Text)) required=(true) maxlength="200"></label>
                            <label>"Kind "
                                <select name="kind" data-type=(kind(Field::Text))>
                                    for (value, text) in [("aired", "As aired"), ("dvd", "As on disc"), ("absolute", "Absolute"), ("custom", "Custom")] {
                                        <option value=(value)>(text)</option>
                                    }
                                </select>
                            </label>
                            <label>"Edition "
                                <select name="edition_id" data-type=(kind(Field::Text))>
                                    for (id, label) in ordering.editions.iter() {
                                        <option value=(id.as_str())>(label.as_str())</option>
                                    }
                                </select>
                            </label>
                        )
                    }
                } else {
                    empty(message: "Play orders are unavailable right now.")
                }
            </section>
        }
    })
}

// --- Collections and playlists ----------------------------------------------

/// The filter conditions the form edits, one slot each: (field, operator,
/// label, value type).
const FILTER_SLOTS: [(&str, &str, &str, &str); 8] = [
    ("kind", "eq", "Kind", "text"),
    ("title", "contains", "Title contains", "text"),
    ("genre", "eq", "Genre", "text"),
    ("tag", "eq", "Tag", "text"),
    ("year", "gte", "Year from", "int"),
    ("year", "lte", "Year to", "int"),
    ("watched", "eq", "Watched", "bool"),
    ("availability", "eq", "Available", "bool"),
];

/// The terms a filter form shows in its slots (the first of each), and the
/// rest, which it carries unchanged so editing never drops a condition.
fn filter_slots(terms: &[FilterTermView]) -> (Vec<Option<String>>, Vec<FilterTermView>) {
    let mut values = vec![None; FILTER_SLOTS.len()];
    let mut rest = Vec::new();
    for term in terms {
        match FILTER_SLOTS
            .iter()
            .position(|(f, o, _, _)| *f == term.field && *o == term.operator)
        {
            Some(slot) if values[slot].is_none() => values[slot] = Some(term.value.clone()),
            _ => rest.push(term.clone()),
        }
    }
    (values, rest)
}

/// A filter's conditions in words.
fn filter_text(terms: &[FilterTermView]) -> String {
    if terms.is_empty() {
        return "Every title".into();
    }
    terms
        .iter()
        .map(
            |t| match (t.field.as_str(), t.operator.as_str(), t.value.as_str()) {
                ("watched", _, "true") => "Watched".into(),
                ("watched", _, _) => "Not watched".into(),
                ("availability", _, "true") => "Available".into(),
                ("availability", _, _) => "Unavailable".into(),
                ("year", "gte", v) => format!("From {v}"),
                ("year", "lte", v) => format!("Until {v}"),
                ("title", "contains", v) => format!("Title contains “{v}”"),
                ("person_id", _, v) => format!("Person {v}"),
                (field, "contains", v) => format!("{} contains “{v}”", capitalized(field)),
                (field, _, v) => format!("{} is {v}", capitalized(field)),
            },
        )
        .collect::<Vec<_>>()
        .join(" · ")
}

fn capitalized(field: &str) -> String {
    let mut chars = field.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The condition fields of a saved filter form, prefilled from `terms`.
#[component]
async fn filter_fields(
    name: String,
    profile: String,
    terms: Vec<FilterTermView>,
) -> Result<impl View> {
    let (values, rest) = filter_slots(&terms);
    let value = move |slot: usize| values[slot].clone().unwrap_or_default();
    Ok(view! {
        <label>"Name " <input name="name" data-type=(kind(Field::Text)) required=(true) maxlength="200" value=(name.as_str())></label>
        <input type="hidden" name="profile_id" data-type=(kind(Field::Text)) value=(profile.as_str())>
        <fieldset class="filter-terms">
            <legend>"Titles that match all of these (leave blank to ignore)"</legend>
            <label>"Kind "
                <select name="all" data-type=(kind(Field::Term)) data-field="kind" data-operator="eq" data-value-type="text">
                    for (v, text) in [("", "Any"), ("movie", "Movie"), ("series", "Series"), ("season", "Season"), ("episode", "Episode"), ("unclassified", "Unclassified")] {
                        <option value=(v) selected=(value(0) == v)>(text)</option>
                    }
                </select>
            </label>
            for slot in 1..4 {
                <label>(FILTER_SLOTS[slot].2) " "
                    <input name="all" data-type=(kind(Field::Term)) data-field=(FILTER_SLOTS[slot].0) data-operator=(FILTER_SLOTS[slot].1) data-value-type="text" maxlength="256" value=(value(slot))>
                </label>
            }
            for slot in 4..6 {
                <label>(FILTER_SLOTS[slot].2) " "
                    <input type="number" min="1" max="9999" name="all" data-type=(kind(Field::Term)) data-field="year" data-operator=(FILTER_SLOTS[slot].1) data-value-type="int" value=(value(slot))>
                </label>
            }
            for slot in 6..8 {
                <label>(FILTER_SLOTS[slot].2) " "
                    <select name="all" data-type=(kind(Field::Term)) data-field=(FILTER_SLOTS[slot].0) data-operator="eq" data-value-type="bool">
                        <option value="" selected=(value(slot).is_empty())>"Either"</option>
                        <option value="true" selected=(value(slot) == "true")>"Yes"</option>
                        <option value="false" selected=(value(slot) == "false")>"No"</option>
                    </select>
                </label>
            }
            if !rest.is_empty() {
                <p class="muted small">(format!("Also kept unchanged: {}.", filter_text(&rest)))</p>
                for term in rest.iter() {
                    <input type="hidden" name="all" data-type=(kind(Field::Term)) data-field=(term.field.as_str()) data-operator=(term.operator.as_str())
                        data-value-type=(match term.field.as_str() { "year" => "int", "watched" | "availability" => "bool", _ => "text" })
                        value=(term.value.as_str())>
                }
            }
        </fieldset>
    })
}

#[page("/organize")]
pub(crate) async fn organize(cx: &Cx) -> Result<impl View> {
    let who = principal(cx)?;
    let model = facade(cx).0.organize(who).await.map_err(ui_error)?;
    let profile = model.profile_id.clone();
    Ok(view! {
        <h1>"Collections and playlists"</h1>
        <p class="muted">"These belong to the current profile. Add titles from a title’s Manage page. Titles you cannot see stay in place when you edit."</p>
        <section aria-labelledby="collections-heading">
            <h2 id="collections-heading">"Collections"</h2>
            if model.collections.is_empty() {
                empty(message: "No collections yet.")
            } else {
                for collection in model.collections {
                    <section class="collection" aria-labelledby=(format!("col-{}", collection.id))>
                        <h3 id=(format!("col-{}", collection.id))>(collection.name.as_str())
                            if collection.kind == "smart" { <span class="muted small">" · smart"</span> }
                        </h3>
                        if collection.kind == "smart" {
                            <p class="muted">
                                (match model.filters.iter().find(|f| Some(&f.id) == collection.filter_id.as_ref()) {
                                    Some(filter) => format!("Titles that match “{}”: {}. Change the conditions under Saved filters.", filter.name, filter_text(&filter.terms)),
                                    None => "Its titles follow a saved filter of another profile or one you cannot read.".to_string(),
                                })
                            </p>
                        } else if collection.items.is_empty() {
                            <p class="muted">"Empty."</p>
                        } else {
                            <ul class="rows">
                                for (item, remove) in collection.items.iter().zip(collection.remove_bodies.iter()) {
                                    <li><a href=(format!("/item/{}", item.id))>(item.title.as_str())</a>
                                        command_form(command: format!("PUT /api/v2/collections/{}", collection.id), label: "Remove", if_match: Some(collection.etag.clone()), json_body: Some(remove.clone()), class: Some("command inline"))
                                    </li>
                                }
                            </ul>
                        }
                        command_form(command: format!("DELETE /api/v2/collections/{}", collection.id), label: "Delete collection", if_match: Some(collection.etag.clone()), class: Some("command inline"))
                    </section>
                }
            }
            <h3>"New collection"</h3>
            command_form(command: "POST /api/v2/collections".to_string(), label: "Create collection", idempotent: true, class: Some("command inline"),
                <label>"Name " <input name="name" data-type=(kind(Field::Text)) required=(true) maxlength="200"></label>
                <input type="hidden" name="profile_id" data-type=(kind(Field::Text)) value=(profile.as_str())>
                <input type="hidden" name="kind" data-type=(kind(Field::Text)) value="manual">
                <input type="hidden" name="item_ids" data-type=(kind(Field::Json)) value="[]">
                <input type="hidden" name="filter_id" data-type=(kind(Field::Json)) value="null">
            )
        </section>
        <section aria-labelledby="filters-heading">
            <h2 id="filters-heading">"Saved filters"</h2>
            <p class="muted">"A saved filter describes titles by their details. Make a smart collection from it to browse them; the collection follows later changes."</p>
            if model.filters.is_empty() {
                empty(message: "No saved filters yet.")
            } else {
                for filter in model.filters.iter() {
                    <section class="collection" aria-labelledby=(format!("flt-{}", filter.id))>
                        <h3 id=(format!("flt-{}", filter.id))>(filter.name.as_str())</h3>
                        <p>(filter_text(&filter.terms))</p>
                        command_form(command: "POST /api/v2/collections".to_string(), label: "Make smart collection", idempotent: true, class: Some("command inline"),
                            json_body: Some(serde_json::json!({"name": filter.name, "profile_id": profile, "kind": "smart", "item_ids": [], "filter_id": filter.id}).to_string()))
                        <details>
                            <summary>"Change conditions"</summary>
                            command_form(command: format!("PUT /api/v2/catalog/filters/{}", filter.id), label: "Save conditions", if_match: Some(filter.etag.clone()), class: Some("command"),
                                filter_fields(name: filter.name.clone(), profile: profile.clone(), terms: filter.terms.clone())
                            )
                        </details>
                        if filter.in_use {
                            <p class="muted small">"A smart collection uses this filter; delete that collection first to delete the filter."</p>
                        } else {
                            command_form(command: format!("DELETE /api/v2/catalog/filters/{}", filter.id), label: "Delete filter", if_match: Some(filter.etag.clone()), class: Some("command inline"))
                        }
                    </section>
                }
            }
            <h3>"New saved filter"</h3>
            command_form(command: "POST /api/v2/catalog/filters".to_string(), label: "Save filter", idempotent: true, class: Some("command"),
                filter_fields(name: String::new(), profile: profile.clone(), terms: Vec::new())
            )
        </section>
        <section aria-labelledby="playlists-heading">
            <h2 id="playlists-heading">"Playlists"</h2>
            if model.playlists.is_empty() {
                empty(message: "No playlists yet.")
            } else {
                for playlist in model.playlists {
                    <section class="collection" aria-labelledby=(format!("pl-{}", playlist.id))>
                        <h3 id=(format!("pl-{}", playlist.id))>(playlist.name.as_str())</h3>
                        if playlist.entries.is_empty() {
                            <p class="muted">"Empty."</p>
                        } else {
                            <ol class="rows">
                                for (entry, (remove, up)) in playlist.entries.iter().zip(playlist.entry_bodies.iter()) {
                                    <li>(entry.title.as_str())
                                        if let Some(up) = up {
                                            command_form(command: format!("PUT /api/v2/playlists/{}", playlist.id), label: "Move up", if_match: Some(playlist.etag.clone()), json_body: Some(up.clone()), class: Some("command inline"))
                                        }
                                        command_form(command: format!("PUT /api/v2/playlists/{}", playlist.id), label: "Remove", if_match: Some(playlist.etag.clone()), json_body: Some(remove.clone()), class: Some("command inline"))
                                    </li>
                                }
                            </ol>
                        }
                        command_form(command: format!("DELETE /api/v2/playlists/{}", playlist.id), label: "Delete playlist", if_match: Some(playlist.etag.clone()), class: Some("command inline"))
                    </section>
                }
            }
            <h3>"New playlist"</h3>
            command_form(command: "POST /api/v2/playlists".to_string(), label: "Create playlist", idempotent: true, class: Some("command inline"),
                <label>"Name " <input name="name" data-type=(kind(Field::Text)) required=(true) maxlength="200"></label>
                <input type="hidden" name="profile_id" data-type=(kind(Field::Text)) value=(profile.as_str())>
                <input type="hidden" name="entries" data-type=(kind(Field::Json)) value="[]">
            )
        </section>
    })
}
