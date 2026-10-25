//! Private whole-file HLS candidate. Not a public planner or route capability.
//! The retained native descriptor is the sole source of encoder bytes. Motion
//! projection rows select identities but never provide filesystem authority.
use super::*;
use crate::v2::{Problem, content, playback};
use motion_catalog_backend::occurrence_bridge::OccurrenceMapping;
use playscale_core::access::{self, Principal};
use sha2::{Digest, Sha256};

pub(crate) type Slot = Arc<Mutex<Option<Arc<Input>>>>;

pub(crate) struct Input {
    owner: Owner,
    version: String,
    native_source: String,
    exact: catabolic_core::catalog_identity::ExactFileRevision,
    binding: serde_json::Value,
    policy: serde_json::Value,
    descriptor: Arc<catabolic_runtime::host::ExactSource>,
    duration_ms: u64,
    outputs: Mutex<HashMap<u64, Arc<OutputFiles>>>,
    // Retain the actual backend as well as its authorization lease. Declared
    // last so descriptor/lease/output owners drop before the runtime owner.
    _runtime: Arc<crate::native_catalog::Runtime>,
}
fn convert(p: Problem) -> ApiError {
    ApiError::new(p.status, p.code, &p.detail)
}
fn changed() -> ApiError {
    ApiError::conflict(
        "source_revision_changed",
        "The exact native HLS representation changed",
    )
}

async fn admitted(
    app: &App,
    tx: &mut sqlx::SqliteConnection,
    principal: &Principal,
    owner: &Owner,
    version: &str,
) -> Result<(i64, OccurrenceMapping), ApiError> {
    let native = app
        .native_catalog
        .as_ref()
        .ok_or_else(ApiError::not_found)?;
    native
        .validate_projection(tx)
        .await
        .map_err(ApiError::internal)?;
    playback::require_native_scope(tx, principal)
        .await
        .map_err(convert)?;
    let facts = content::transport_file_facts(tx, principal, &owner.file)
        .await
        .map_err(convert)?;
    if !playscale_core::playback_session::may_control(owner, principal, facts.as_ref())
        || facts
            .as_ref()
            .is_none_or(|f| f.revision != owner.file_revision)
        || !content::transport_version_ready(tx, principal, version)
            .await
            .map_err(convert)?
    {
        return Err(ApiError::not_found());
    }
    let exact: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM media_versions v JOIN version_files b ON b.version_id=v.id WHERE v.id=? AND v.timeline_id=? AND v.origin IN ('original','generated') AND b.part=1 AND b.file_id=? AND b.file_revision=? AND b.start_ms IS NULL AND b.end_ms IS NULL AND (SELECT count(*) FROM version_files other WHERE other.version_id=v.id)=1)")
        .bind(version).bind(&owner.timeline).bind(&owner.file).bind(&owner.file_revision).fetch_one(&mut *tx).await?;
    if !exact {
        return Err(changed());
    }
    let (generation, raw): (i64, String) = sqlx::query_as("SELECT s.generation,p.mapping_json FROM catabolic_projection_files p JOIN catabolic_projection_state s ON s.singleton=1 AND s.generation=p.generation WHERE p.file_id=? AND p.available=1")
        .bind(&owner.file).fetch_one(&mut *tx).await?;
    if raw.len() > 4 * 1024 * 1024 {
        return Err(changed());
    }
    let mapping: OccurrenceMapping = serde_json::from_str(&raw).map_err(ApiError::internal)?;
    if mapping.motion_file["id"] != owner.file
        || mapping.motion_file["revision"] != owner.file_revision
    {
        return Err(changed());
    }
    if !cfg!(target_os = "linux")
        || core::native_hls_media_duration(
            mapping.motion_file["duration_seconds"].as_f64(),
            mapping.motion_file["bytes"].as_i64(),
        )
        .is_none()
    {
        return Err(unsupported());
    }
    Ok((generation, mapping))
}

/// Only the detached delivery admission owner calls this while retaining its
/// writer. Opening a source has no authority from a supplied path or plan JSON.
pub(crate) async fn open_in(
    app: &App,
    tx: &mut sqlx::SqliteConnection,
    principal: &Principal,
    owner: Owner,
    version: String,
) -> Result<Arc<Input>, ApiError> {
    let (generation, mapping) = admitted(app, tx, principal, &owner, &version).await?;
    let duration_ms = core::native_hls_media_duration(
        mapping.motion_file["duration_seconds"].as_f64(),
        mapping.motion_file["bytes"].as_i64(),
    )
    .ok_or_else(unsupported)?;
    let native_source = mapping.source.native_source.clone();
    let exact = mapping.physical.exact.clone();
    let binding = mapping.source.native_binding.clone();
    let policy = mapping.source.native_policy.clone();
    let descriptor = app
        .native_catalog
        .as_ref()
        .ok_or_else(ApiError::not_found)?
        .with_host(move |host| {
            anyhow::ensure!(
                host.source_binding(&mapping.source.native_source)?
                    == mapping.source.native_binding
                    && host.source_observation_policy(&mapping.source.native_source)?
                        == mapping.source.native_policy,
                "native HLS source binding or policy changed"
            );
            let source = host.open_exact_source(
                generation,
                &mapping.source.native_source,
                &mapping.physical.exact,
            )?;
            source_header(&source.descriptor()?)?;
            Ok(source)
        })
        .await
        .map_err(ApiError::internal)?;
    Ok(Arc::new(Input {
        owner,
        version,
        native_source,
        exact,
        binding,
        policy,
        descriptor: Arc::new(descriptor),
        duration_ms,
        outputs: Mutex::default(),
        _runtime: app
            .native_catalog
            .as_ref()
            .ok_or_else(ApiError::not_found)?
            .clone(),
    }))
}
impl Input {
    pub(super) fn descriptor(&self) -> anyhow::Result<std::fs::File> {
        Ok(self.descriptor.descriptor()?)
    }
    pub(super) fn matches(&self, owner: &Owner, version: &str) -> bool {
        self.owner == *owner && self.version == version
    }
    pub(super) fn discard(&self, generation: u64, below: Option<u32>) {
        let mut generations = self.outputs.lock().unwrap();
        if let Some(below) = below {
            if let Some(files) = generations.get(&generation) {
                files.pins.lock().unwrap().retain(|name, _| {
                    name == "init.mp4"
                        || name
                            .strip_prefix("seg")
                            .and_then(|n| n.strip_suffix(".m4s"))
                            .and_then(|n| n.parse::<u32>().ok())
                            .is_some_and(|n| n >= below)
                });
            }
        } else {
            generations.remove(&generation);
        }
    }
    pub(super) fn register(&self, generation: u64, files: Arc<OutputFiles>) -> anyhow::Result<()> {
        let mut generations = self.outputs.lock().unwrap();
        anyhow::ensure!(
            !generations.contains_key(&generation),
            "native generation output already issued"
        );
        generations.insert(generation, files);
        Ok(())
    }
}

/// Fresh continuation checks do not substitute for request credential/ticket
/// admission, which remains in v2. In particular All-scoped prepared authority
/// is checked on the output itself, never on its now-offline original.
pub(super) async fn validate(app: &App, input: &Arc<Input>) -> Result<(), ApiError> {
    let mut tx = playback::begin_catalog_application(app, false)
        .await
        .map_err(convert)?;
    let device = crate::v2::auth::load_device(&mut tx, &input.owner.principal)
        .await
        .map_err(convert)?;
    let household = if app.access.mode == access::AccessMode::TrustedHousehold {
        Some(
            crate::v2::auth::load_household_scope(&mut tx)
                .await
                .map_err(convert)?,
        )
    } else {
        None
    };
    let principal = playscale_core::playback_session::continuation_principal(
        &input.owner,
        device.as_ref().map(|d| &d.device),
        household
            .as_ref()
            .map(|(library_ids, profile_ids)| access::HouseholdScope {
                library_ids,
                profile_ids,
            }),
    )
    .ok_or_else(ApiError::not_found)?;
    validate_in(app, input, &principal, tx).await.map(drop)
}

async fn validate_in(
    app: &App,
    input: &Arc<Input>,
    principal: &Principal,
    mut tx: sqlx::Transaction<'static, sqlx::Sqlite>,
) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, ApiError> {
    let (generation, mapping) =
        admitted(app, &mut tx, principal, &input.owner, &input.version).await?;
    if mapping.source.native_source != input.native_source
        || mapping.physical.exact != input.exact
        || mapping.source.native_binding != input.binding
        || mapping.source.native_policy != input.policy
    {
        return Err(changed());
    }
    let descriptor = input.descriptor.clone();
    app.native_catalog
        .as_ref()
        .ok_or_else(ApiError::not_found)?
        .with_host(move |host| {
            // The actual blocking owner keeps the writer even if its caller is
            // cancelled. On success that same fence returns to response custody.
            host.validate_exact_source(generation, &descriptor)?;
            Ok(tx)
        })
        .await
        .map_err(ApiError::internal)
}

pub(super) async fn release_fence(
    app: &App,
    id: &str,
    generation: &str,
    principal: &Principal,
    tx: sqlx::Transaction<'static, sqlx::Sqlite>,
) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, ApiError> {
    let session = app.processing.deliveries.session(id)?;
    let input = session
        .native_input
        .as_ref()
        .ok_or_else(ApiError::not_found)?;
    let generation: u64 = generation.parse().map_err(|_| ApiError::not_found())?;
    let tx = validate_in(app, input, principal, tx).await?;
    if !session
        .inner
        .lock()
        .unwrap()
        .delivery
        .serves(generation, app.processing.deliveries.now_ms())
    {
        return Err(ApiError::not_found());
    }
    Ok(tx)
}

// The first commitment is to descriptors created by the actual worker
// preparation, never to names found in its output directory. Each generation
// has one anonymous media file and one anonymous manifest file. The supervisor
// inherits them as fd 5/6; readers use pread, never shared OFD seek offsets.
pub(super) struct OutputFiles {
    pub(super) media: std::fs::File,
    pub(super) manifest: std::fs::File,
    pins: Mutex<HashMap<String, Output>>,
    _capacity: tokio::sync::OwnedSemaphorePermit,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Output {
    offset: u64,
    length: u64,
    digest: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Range {
    offset: u64,
    length: u64,
}
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    range: Range,
    duration_ms: u64,
}
#[derive(Debug, PartialEq, Eq)]
struct Manifest {
    init: Range,
    segments: Vec<Entry>,
    ended: bool,
}

pub(super) const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
pub(super) const MAX_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_NATIVE_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ENTRIES: usize = 16;

fn unsupported() -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "route_unqualified",
        "Private native HLS requires a whole MP4-style source of at most 60 seconds and 64 MiB",
    )
}
fn busy() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "native_hls_busy",
        "Native HLS buffered I/O capacity is busy",
    )
}

/// Linux O_TMPFILE is deliberately required. There is no named-file fallback,
/// no first-observation pathname commitment, and no additional dependency.
#[cfg(target_os = "linux")]
pub(super) fn prepare(
    directory: &FsPath,
    capacity: tokio::sync::OwnedSemaphorePermit,
) -> anyhow::Result<Arc<OutputFiles>> {
    use std::os::unix::fs::OpenOptionsExt;
    let anonymous = || {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_TMPFILE | libc::O_CLOEXEC)
            .mode(0o600)
            .open(directory)
    };
    Ok(Arc::new(OutputFiles {
        media: anonymous()?,
        manifest: anonymous()?,
        pins: Mutex::default(),
        _capacity: capacity,
    }))
}

#[cfg(not(target_os = "linux"))]
pub(super) fn prepare(
    _: &FsPath,
    _: tokio::sync::OwnedSemaphorePermit,
) -> anyhow::Result<Arc<OutputFiles>> {
    anyhow::bail!("anonymous native HLS outputs require Linux")
}

/// The hard per-file output limit applies in the actual supervisor and every
/// child, including malformed output. Accepted manifests have a much lower
/// 64 KiB read limit. Reaching this limit is failure, never successful truncation.
pub(super) fn configure(command: &mut Command) {
    // SAFETY: setrlimit is the only syscall in this pre-exec callback; no
    // allocator, lock or destructor is used between fork and exec.
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: MAX_OUTPUT_BYTES as libc::rlim_t,
                rlim_max: MAX_OUTPUT_BYTES as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

pub(super) fn arguments(args: &mut Vec<String>) -> anyhow::Result<()> {
    let input = args
        .iter()
        .position(|arg| arg == "-i")
        .ok_or_else(|| anyhow::anyhow!("missing native input"))?;
    args.splice(
        input..input,
        ["-f", "mov", "-enable_drefs", "0", "-use_absolute_path", "0"].map(str::to_owned),
    );
    let output = args
        .windows(2)
        .position(|pair| pair == ["-f", "hls"])
        .ok_or_else(|| anyhow::anyhow!("missing HLS muxer"))?;
    args.truncate(output);
    args.extend(
        [
            "-f",
            "hls",
            "-hls_list_size",
            "0",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_type",
            "fmp4",
            "-hls_flags",
            "single_file+independent_segments",
            "-start_number",
            "0",
            "-hls_time",
            "4",
            "-hls_segment_filename",
            "/dev/fd/5",
            "/dev/fd/6",
        ]
        .map(str::to_owned),
    );
    Ok(())
}

fn source_header(source: &std::fs::File) -> anyhow::Result<()> {
    use std::os::unix::fs::FileExt;
    let mut header = [0u8; 16];
    source.read_exact_at(&mut header, 0)?;
    let length = u32::from_be_bytes(header[..4].try_into().unwrap());
    anyhow::ensure!(
        &header[4..8] == b"ftyp"
            && (16..=4096).contains(&length)
            && source.metadata()?.len() <= MAX_SOURCE_BYTES,
        "source is not in the bounded MP4 cohort"
    );
    Ok(())
}

fn parse_range(text: &str, maximum: u64) -> Option<Range> {
    let (length, offset) = text.split_once('@')?;
    let range = Range {
        length: length.parse().ok()?,
        offset: offset.parse().ok()?,
    };
    (range.length > 0
        && range.length <= maximum
        && range.offset.checked_add(range.length)? <= MAX_OUTPUT_BYTES)
        .then_some(range)
}

/// Only the bounded single-file recipe grammar is accepted. URI text is never
/// opened: it must name the known inherited output, and all ranges are read
/// from the parent-created descriptor. No implicit offsets, key URLs or gaps.
fn parse_manifest(text: &str) -> Option<Manifest> {
    if text.len() > MAX_MANIFEST_BYTES as usize
        || !text.starts_with("#EXTM3U\n")
        || !text.ends_with('\n')
    {
        return None;
    }
    let mut init = None;
    let mut segments = Vec::new();
    let mut duration = None;
    let mut range = None;
    let mut ended = false;
    let mut zero_sequence = false;
    let mut independent = false;
    let mut vod = false;
    for line in text.lines().skip(1) {
        if line.is_empty() {
            continue;
        }
        if ended {
            return None;
        }
        if let Some(value) = line.strip_prefix("#EXT-X-MAP:URI=\"") {
            if init.is_some() {
                return None;
            }
            let (uri, tail) = value.split_once("\",BYTERANGE=\"")?;
            if !matches!(uri, "5" | "/dev/fd/5") {
                return None;
            }
            let value = parse_range(tail.strip_suffix('"')?, MAX_INIT_BYTES)?;
            if value.offset != 0 {
                return None;
            }
            init = Some(value);
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            if duration.is_some() || range.is_some() {
                return None;
            }
            let seconds = value.strip_suffix(',')?.parse::<f64>().ok()?;
            if !seconds.is_finite() || seconds <= 0.0 || seconds > TARGET_SECONDS as f64 {
                return None;
            }
            duration = Some((seconds * 1000.0).round() as u64);
        } else if let Some(value) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            if duration.is_none() || range.is_some() {
                return None;
            }
            range = Some(parse_range(value, MAX_NATIVE_SEGMENT_BYTES)?);
        } else if line == "#EXT-X-ENDLIST" {
            if duration.is_some() || range.is_some() {
                return None;
            }
            ended = true;
        } else if line == "#EXT-X-PLAYLIST-TYPE:VOD" {
            if vod {
                return None;
            }
            vod = true;
        } else if line == "#EXT-X-INDEPENDENT-SEGMENTS" {
            independent = true;
        } else if line == "#EXT-X-MEDIA-SEQUENCE:0" {
            if zero_sequence {
                return None;
            }
            zero_sequence = true;
        } else if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            if !(1..=TARGET_SECONDS).contains(&value.parse::<u64>().ok()?) {
                return None;
            }
        } else if let Some(value) = line.strip_prefix("#EXT-X-VERSION:") {
            if !(7..=10).contains(&value.parse::<u64>().ok()?) {
                return None;
            }
        } else if matches!(line, "5" | "/dev/fd/5") {
            let entry = Entry {
                range: range.take()?,
                duration_ms: duration.take()?,
            };
            let expected = segments
                .last()
                .map(|e: &Entry| e.range.offset + e.range.length)
                .unwrap_or(init.as_ref()?.length);
            if entry.range.offset != expected || segments.len() >= MAX_ENTRIES {
                return None;
            }
            segments.push(entry);
        } else {
            return None;
        }
    }
    if !zero_sequence
        || !independent
        || !vod
        || duration.is_some()
        || range.is_some()
        || segments.is_empty()
    {
        return None;
    }
    Some(Manifest {
        init: init?,
        segments,
        ended,
    })
}

fn read_range(file: &std::fs::File, range: &Range, limit: u64) -> Result<Vec<u8>, ApiError> {
    use std::os::unix::fs::FileExt;
    let end = range.offset.checked_add(range.length).ok_or_else(changed)?;
    let length = file.metadata().map_err(ApiError::internal)?.len();
    if range.length == 0 || range.length > limit || length > MAX_OUTPUT_BYTES || end > length {
        return Err(changed());
    }
    let mut bytes = vec![0; usize::try_from(range.length).map_err(ApiError::internal)?];
    file.read_exact_at(&mut bytes, range.offset)
        .map_err(ApiError::internal)?;
    Ok(bytes)
}
fn commitment(range: &Range, bytes: &[u8]) -> Output {
    Output {
        offset: range.offset,
        length: range.length,
        digest: Sha256::digest(bytes).into(),
    }
}
fn commit(files: &OutputFiles, name: String, output: Output) -> Result<(), ApiError> {
    let mut pins = files.pins.lock().unwrap();
    match pins.get(&name) {
        Some(old) if old != &output => Err(changed()),
        Some(_) => Ok(()),
        None => {
            pins.insert(name, output);
            Ok(())
        }
    }
}
fn read_pinned(files: &OutputFiles, name: &str) -> Result<Vec<u8>, ApiError> {
    let pin = files
        .pins
        .lock()
        .unwrap()
        .get(name)
        .cloned()
        .ok_or_else(ApiError::not_found)?;
    let range = Range {
        offset: pin.offset,
        length: pin.length,
    };
    let bytes = read_range(
        &files.media,
        &range,
        if name == "init.mp4" {
            MAX_INIT_BYTES
        } else {
            MAX_NATIVE_SEGMENT_BYTES
        },
    )?;
    if commitment(&range, &bytes) != pin {
        return Err(changed());
    }
    Ok(bytes)
}
fn free_space(file: &std::fs::File) -> std::io::Result<u64> {
    use std::os::fd::AsRawFd;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: fstatvfs initializes stat on success; the retained file owns fd.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

/// Bound allocations in the shared IDR parser too, not only its input Vec:
/// one <=256 KiB moof, <=64 total boxes/mdat ranges, and its 64 KiB reader.
fn bounded_fragment(bytes: &[u8]) -> bool {
    let mut rest = bytes;
    let (mut boxes, mut moofs, mut media) = (0usize, 0usize, 0usize);
    while !rest.is_empty() {
        if rest.len() < 8 || boxes == 64 {
            return false;
        }
        boxes += 1;
        let size = u32::from_be_bytes(rest[..4].try_into().unwrap());
        let (header, size) = match size {
            0 => return false,
            1 if rest.len() >= 16 => (16u64, u64::from_be_bytes(rest[8..16].try_into().unwrap())),
            1 => return false,
            size => (8, u64::from(size)),
        };
        if size < header || size > rest.len() as u64 {
            return false;
        }
        match &rest[4..8] {
            b"moof" => {
                moofs += 1;
                if moofs != 1 || size > 256 * 1024 {
                    return false;
                }
            }
            b"mdat" => media += 1,
            _ => (),
        }
        rest = &rest[size as usize..];
    }
    moofs == 1 && media > 0
}

fn inspect(files: &OutputFiles, reported: u32, floor: u64) -> Result<Option<Manifest>, ApiError> {
    use std::os::unix::fs::FileExt;
    if free_space(&files.media).map_err(ApiError::internal)? < floor {
        return Err(changed());
    }
    let before = files.manifest.metadata().map_err(ApiError::internal)?;
    if before.len() == 0 {
        return Ok(None);
    }
    if before.len() > MAX_MANIFEST_BYTES
        || files.media.metadata().map_err(ApiError::internal)?.len() > MAX_OUTPUT_BYTES
    {
        return Err(changed());
    }
    let mut text = vec![0; before.len() as usize];
    // A concurrent truncate/rewrite is a retry, not output authority. A stable
    // malformed snapshot is also withheld until the normal progress deadline.
    if files.manifest.read_exact_at(&mut text, 0).is_err()
        || scan::fingerprint(&before)
            != scan::fingerprint(&files.manifest.metadata().map_err(ApiError::internal)?)
    {
        return Ok(None);
    }
    let Some(manifest) = std::str::from_utf8(&text).ok().and_then(parse_manifest) else {
        return Ok(None);
    };
    if manifest.segments.len() < reported as usize {
        return Err(changed());
    }
    let init = read_range(&files.media, &manifest.init, MAX_INIT_BYTES)?;
    let track = crate::fmp4::video_track(&init).ok_or_else(changed)?;
    commit(files, "init.mp4".into(), commitment(&manifest.init, &init))?;
    for (index, entry) in manifest.segments.iter().enumerate().skip(reported as usize) {
        let bytes = read_range(&files.media, &entry.range, MAX_NATIVE_SEGMENT_BYTES)?;
        if !bounded_fragment(&bytes)
            || crate::fmp4::starts_with_idr(track, &mut std::io::Cursor::new(&bytes)) != Some(true)
        {
            return Err(changed());
        }
        commit(
            files,
            format!("seg{index}.m4s"),
            commitment(&entry.range, &bytes),
        )?;
    }
    Ok(Some(manifest))
}

pub(super) async fn observe(
    app: &App,
    session: &Arc<Session>,
    generation: u64,
    reported: &mut u32,
    start_ms: u64,
    lease: &Arc<execution::Lease>,
    successful_exit: bool,
) -> bool {
    let Some(input) = &session.native_input else {
        return false;
    };
    if validate(app, input).await.is_err() {
        let _ = apply(app, session, super::Input::Close);
        return false;
    }
    let Some(files) = input.outputs.lock().unwrap().get(&generation).cloned() else {
        return false;
    };
    let capacity = if successful_exit {
        // A final observation gets the enclosing bounded deadline to wait for
        // a publication reader; transient contention is not an encoder error.
        match app
            .processing
            .deliveries
            .native_observe_slots
            .clone()
            .acquire_owned()
            .await
        {
            Ok(capacity) => capacity,
            Err(_) => return false,
        }
    } else {
        match app
            .processing
            .deliveries
            .native_observe_slots
            .clone()
            .try_acquire_owned()
        {
            Ok(capacity) => capacity,
            Err(_) => return false,
        }
    };
    let (retained, ownership, from, floor) = (
        session.clone(),
        lease.clone(),
        *reported,
        app.storage.settings.min_free_bytes,
    );
    let inspected = tokio::task::spawn_blocking(move || {
        let (_retained, _ownership, _capacity) = (retained, ownership, capacity);
        if successful_exit {
            inspect(&files, from, floor)
        } else if free_space(&files.media).map_err(ApiError::internal)? < floor {
            Err(changed())
        } else {
            Ok(None)
        }
    })
    .await;
    let snapshot = match inspected {
        Ok(Ok(Some(snapshot))) => snapshot,
        Ok(Ok(None)) => return false,
        _ => {
            let _ = apply(app, session, super::Input::Failed { generation });
            return false;
        }
    };
    if validate(app, input).await.is_err() {
        let _ = apply(app, session, super::Input::Close);
        return false;
    }
    let durations: Vec<u64> = snapshot
        .segments
        .iter()
        .map(|entry| entry.duration_ms)
        .collect();
    if !successful_exit
        || !snapshot.ended
        || from != 0
        || !core::native_hls_complete(input.duration_ms.saturating_sub(start_ms), &durations)
    {
        let _ = apply(app, session, super::Input::Failed { generation });
        return false;
    }
    // This bounded VOD cohort consumes exactly one final manifest only after
    // successful supervisor exit AND witness confirmation. Nothing is exposed
    // from a mutable progressive prefix or a truncated output.
    for (index, entry) in snapshot.segments.iter().enumerate() {
        let event = if index == 0 {
            super::Input::Ready {
                generation,
                media_time_origin_ms: start_ms,
                first_segment_start_ms: 0,
                first_segment_ms: entry.duration_ms,
                target_duration_s: TARGET_SECONDS,
            }
        } else {
            super::Input::Segment {
                generation,
                index: index as u32,
                duration_ms: entry.duration_ms,
                now_ms: app.processing.deliveries.now_ms(),
            }
        };
        if apply(app, session, event).is_err() {
            return false;
        }
        *reported += 1;
    }
    let _ = apply(
        app,
        session,
        super::Input::Completed {
            generation,
            final_index: *reported - 1,
        },
    );
    true
}

pub(super) async fn codecs(
    app: &App,
    session: Arc<Session>,
    generation: u64,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<Option<(String, Option<(u32, u32)>)>, ApiError> {
    let input = session
        .native_input
        .as_ref()
        .ok_or_else(ApiError::not_found)?
        .clone();
    let files = input
        .outputs
        .lock()
        .unwrap()
        .get(&generation)
        .cloned()
        .ok_or_else(ApiError::not_found)?;
    let capacity = app
        .processing
        .deliveries
        .native_buffer_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| busy())?;
    let retained = session.clone();
    let value = tokio::task::spawn_blocking(move || {
        let (_permit, _session, _capacity) = (permit, retained, capacity);
        let bytes = read_pinned(&files, "init.mp4")?;
        Ok::<_, ApiError>(
            crate::fmp4::codecs(&bytes).map(|codecs| (codecs, crate::fmp4::video_size(&bytes))),
        )
    })
    .await
    .map_err(ApiError::internal)??;
    validate(app, &input).await?;
    Ok(value)
}

/// Guards belong to the allocation, not its body producer: HTTP consumers may
/// retain, clone or slice a yielded Bytes after the producer reaches EOF.
struct BufferedBytes<Ownership> {
    bytes: Vec<u8>,
    _ownership: Ownership,
}
impl<Ownership> AsRef<[u8]> for BufferedBytes<Ownership> {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
fn owned_body<Ownership: Send + 'static>(bytes: Vec<u8>, ownership: Ownership) -> axum::body::Body {
    axum::body::Body::from(axum::body::Bytes::from_owner(BufferedBytes {
        bytes,
        _ownership: ownership,
    }))
}

pub(super) fn text_body(
    app: &App,
    session: Arc<Session>,
    permit: tokio::sync::OwnedSemaphorePermit,
    text: String,
) -> Result<axum::body::Body, ApiError> {
    if session.native_input.is_none() || text.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(ApiError::not_found());
    }
    let capacity = app
        .processing
        .deliveries
        .native_buffer_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| busy())?;
    Ok(owned_body(text.into_bytes(), (session, permit, capacity)))
}

pub(super) async fn serve_output(
    app: &App,
    session: Arc<Session>,
    generation: u64,
    name: String,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<Response, ApiError> {
    let input = session
        .native_input
        .as_ref()
        .ok_or_else(ApiError::not_found)?
        .clone();
    let files = input
        .outputs
        .lock()
        .unwrap()
        .get(&generation)
        .cloned()
        .ok_or_else(ApiError::not_found)?;
    let capacity = app
        .processing
        .deliveries
        .native_buffer_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| busy())?;
    let retained = session.clone();
    let (bytes, permit, capacity) = tokio::task::spawn_blocking(move || {
        let _retained = retained;
        let bytes = read_pinned(&files, &name)?;
        Ok::<_, ApiError>((bytes, permit, capacity))
    })
    .await
    .map_err(ApiError::internal)??;
    validate(app, &input).await?;
    if !session
        .inner
        .lock()
        .unwrap()
        .delivery
        .serves(generation, app.processing.deliveries.now_ms())
    {
        return Err(ApiError::not_found());
    }
    let body = owned_body(bytes, (session, permit, capacity));
    Ok((stream_headers("video/mp4"), body).into_response())
}

#[cfg(all(test, target_os = "linux"))]
#[path = "native_hls_tests.rs"]
mod tests;
