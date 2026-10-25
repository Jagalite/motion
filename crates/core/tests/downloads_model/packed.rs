//! Test-only retained-state representation, not a different model or explorer.
//! Every Eq field of World and its nested production structs derives serde.
//! The only elisions are Option::None fields, which deserialize to exactly None.
//! There are no ignored fields or floating values. Objects sort keys explicitly.
use super::*;
use serde_json::Value;
#[path = "packed_stream.rs"]
mod streaming;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

// These are compression tokens, not semantic domains: all other UTF8 strings
// retain their exact bytes through the literal escape, including empty strings.
const DICT: &[&str] = &[
    "view",
    "timeline_revision",
    "next_sequence",
    "pruned",
    "recorded",
    "lineage",
    "download",
    "base",
    "log",
    "base_epoch",
    "first",
    "other_viewing",
    "other_manual",
    "online",
    "manual",
    "edits",
    "epoch",
    "restores",
    "restored_base",
    "viewing_since_restore",
    "manual_since_restore",
    "revision",
    "manual_epoch",
    "position_ms",
    "automatic_watched",
    "manual_watched",
    "session",
    "event",
    "outcome",
    "code",
    "id",
    "sequence",
    "download_id",
    "timeline_id",
    "base_viewing_revision",
    "base_manual_epoch",
    "status",
    "device_time",
    "produced_revision",
    "source_revision",
    "viewing_epoch",
    "scope",
    "media",
    "event_id",
    "device_sequence",
    "server_id",
    "principal_id",
    "profile_id",
    "device_id",
    "server",
    "device",
    "profile",
    "timeline",
    "source",
    "paused",
    "ended",
    "applied",
    "history_only",
    "rejected",
    "not_found",
    "invalid_event",
    "invalid_position",
    "timeline_changed",
    "manual_override_newer",
    "viewing_newer",
    "server:0",
    "server:1",
    "e1",
    "e2",
    "e3",
    "e4",
    "t1",
    "t2",
    "t3",
    "t4",
    "gap",
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "10",
];
fn token(value: &str) -> Option<usize> {
    match value {
        "view" => Some(0),
        "timeline_revision" => Some(1),
        "next_sequence" => Some(2),
        "pruned" => Some(3),
        "recorded" => Some(4),
        "lineage" => Some(5),
        "download" => Some(6),
        "base" => Some(7),
        "log" => Some(8),
        "base_epoch" => Some(9),
        "first" => Some(10),
        "other_viewing" => Some(11),
        "other_manual" => Some(12),
        "online" => Some(13),
        "manual" => Some(14),
        "edits" => Some(15),
        "epoch" => Some(16),
        "restores" => Some(17),
        "restored_base" => Some(18),
        "viewing_since_restore" => Some(19),
        "manual_since_restore" => Some(20),
        "revision" => Some(21),
        "manual_epoch" => Some(22),
        "position_ms" => Some(23),
        "automatic_watched" => Some(24),
        "manual_watched" => Some(25),
        "session" => Some(26),
        "event" => Some(27),
        "outcome" => Some(28),
        "code" => Some(29),
        "id" => Some(30),
        "sequence" => Some(31),
        "download_id" => Some(32),
        "timeline_id" => Some(33),
        "base_viewing_revision" => Some(34),
        "base_manual_epoch" => Some(35),
        "status" => Some(36),
        "device_time" => Some(37),
        "produced_revision" => Some(38),
        "source_revision" => Some(39),
        "viewing_epoch" => Some(40),
        "scope" => Some(41),
        "media" => Some(42),
        "event_id" => Some(43),
        "device_sequence" => Some(44),
        "server_id" => Some(45),
        "principal_id" => Some(46),
        "profile_id" => Some(47),
        "device_id" => Some(48),
        "server" => Some(49),
        "device" => Some(50),
        "profile" => Some(51),
        "timeline" => Some(52),
        "source" => Some(53),
        "paused" => Some(54),
        "ended" => Some(55),
        "applied" => Some(56),
        "history_only" => Some(57),
        "rejected" => Some(58),
        "not_found" => Some(59),
        "invalid_event" => Some(60),
        "invalid_position" => Some(61),
        "timeline_changed" => Some(62),
        "manual_override_newer" => Some(63),
        "viewing_newer" => Some(64),
        "server:0" => Some(65),
        "server:1" => Some(66),
        "e1" => Some(67),
        "e2" => Some(68),
        "e3" => Some(69),
        "e4" => Some(70),
        "t1" => Some(71),
        "t2" => Some(72),
        "t3" => Some(73),
        "t4" => Some(74),
        "gap" => Some(75),
        "0" => Some(76),
        "1" => Some(77),
        "2" => Some(78),
        "3" => Some(79),
        "4" => Some(80),
        "5" => Some(81),
        "6" => Some(82),
        "7" => Some(83),
        "8" => Some(84),
        "9" => Some(85),
        "10" => Some(86),
        _ => None,
    }
}

fn err(message: &str) -> ModelError {
    ModelError::new(message)
}
fn var(mut n: u64, out: &mut Vec<u8>) {
    while n >= 128 {
        out.push((n as u8 & 127) | 128);
        n >>= 7;
    }
    out.push(n as u8);
}
fn string(s: &str, out: &mut Vec<u8>) {
    if let Some(index) = token(s) {
        out.push(4);
        var(index as u64, out);
    } else {
        out.push(5);
        var(s.len() as u64, out);
        out.extend_from_slice(s.as_bytes());
    }
}
fn write(v: &Value, out: &mut Vec<u8>) -> Result<(), ModelError> {
    match v {
        Value::Null => out.push(0),
        Value::Bool(false) => out.push(1),
        Value::Bool(true) => out.push(2),
        Value::Number(n) => {
            if let Some(n) = n.as_u64() {
                out.push(3);
                var(n, out);
            } else if let Some(n) = n.as_i64() {
                out.push(8);
                var(n.unsigned_abs(), out);
            } else {
                return Err(err("packed model does not support floating numbers"));
            }
        }
        Value::String(s) => string(s, out),
        Value::Array(a) => {
            out.push(6);
            var(a.len() as u64, out);
            for v in a {
                write(v, out)?;
            }
        }
        Value::Object(o) => {
            out.push(7);
            var(o.len() as u64, out);
            let mut keys: Vec<_> = o.keys().collect();
            keys.sort_unstable();
            for key in keys {
                string(key, out);
                write(&o[key], out)?;
            }
        }
    }
    Ok(())
}
fn encode_value(value: &Value) -> Result<Vec<u8>, ModelError> {
    let mut bytes = vec![1];
    write(value, &mut bytes)?;
    Ok(bytes)
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, ModelError> {
        let n = *self
            .bytes
            .get(self.at)
            .ok_or_else(|| err("truncated packed state"))?;
        self.at += 1;
        Ok(n)
    }
    fn var(&mut self) -> Result<u64, ModelError> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let b = self.byte()?;
            if shift == 63 && b > 1 {
                return Err(err("packed integer overflow"));
            }
            value |= u64::from(b & 127) << shift;
            if b & 128 == 0 {
                if shift > 0 && b == 0 {
                    return Err(err("noncanonical packed integer"));
                }
                return Ok(value);
            }
        }
        Err(err("packed integer overflow"))
    }
    fn len(&mut self) -> Result<usize, ModelError> {
        let n = usize::try_from(self.var()?).map_err(|_| err("packed length overflow"))?;
        if n > self.bytes.len().saturating_sub(self.at) {
            return Err(err("packed length exceeds input"));
        }
        Ok(n)
    }
    fn value(&mut self, depth: usize) -> Result<Value, ModelError> {
        if depth > 128 {
            return Err(err("packed nesting exceeds model contract"));
        }
        Ok(match self.byte()? {
            0 => Value::Null,
            1 => Value::Bool(false),
            2 => Value::Bool(true),
            3 => Value::from(self.var()?),
            4 => Value::String(
                DICT.get(
                    usize::try_from(self.var()?).map_err(|_| err("dictionary index overflow"))?,
                )
                .ok_or_else(|| err("unknown dictionary token"))?
                .to_string(),
            ),
            5 => {
                let n = self.len()?;
                let end = self.at + n;
                let s = std::str::from_utf8(&self.bytes[self.at..end])
                    .map_err(|_| err("invalid packed UTF8"))?
                    .to_owned();
                self.at = end;
                if token(&s).is_some() {
                    return Err(err("noncanonical literal dictionary string"));
                }
                Value::String(s)
            }
            6 => {
                let n = self.len()?;
                let mut a = Vec::with_capacity(n);
                for _ in 0..n {
                    a.push(self.value(depth + 1)?);
                }
                Value::Array(a)
            }
            7 => {
                let n = self.len()?;
                let mut map = serde_json::Map::new();
                let mut previous: Option<String> = None;
                for _ in 0..n {
                    let Value::String(key) = self.value(depth + 1)? else {
                        return Err(err("non-string packed key"));
                    };
                    if previous.as_ref().is_some_and(|p| p >= &key) {
                        return Err(err("noncanonical packed key order"));
                    }
                    previous = Some(key.clone());
                    map.insert(key, self.value(depth + 1)?);
                }
                Value::Object(map)
            }
            8 => {
                let magnitude = self.var()?;
                if magnitude == 0 || magnitude > (1u64 << 63) {
                    return Err(err("invalid packed negative integer"));
                }
                Value::from(if magnitude == 1u64 << 63 {
                    i64::MIN
                } else {
                    -(magnitude as i64)
                })
            }
            _ => return Err(err("unknown packed tag")),
        })
    }
}
fn decode_value(bytes: &[u8]) -> Result<Value, ModelError> {
    let mut reader = Reader { bytes, at: 0 };
    if reader.byte()? != 1 {
        return Err(err("unknown packed version"));
    }
    let value = reader.value(0)?;
    if reader.at != bytes.len() {
        return Err(err("noncanonical or trailing packed bytes"));
    }
    Ok(value)
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct PackedWorld(Box<[u8]>);
impl PackedWorld {
    fn encode(world: &World) -> Result<Self, ModelError> {
        Self::encode_checked(world).map(|(packed, _)| packed)
    }
    fn encode_checked(world: &World) -> Result<(Self, Rc<World>), ModelError> {
        Self::encode_checked_with::<true>(world)
    }
    fn encode_checked_with<const STREAM: bool>(
        world: &World,
    ) -> Result<(Self, Rc<World>), ModelError> {
        let value = serde_json::to_value(world).map_err(|e| err(&e.to_string()))?;
        let packed = Self(encode_value(&value)?.into_boxed_slice());
        // Checked for EVERY initial state and successor, not only sampled tests.
        let decoded = Rc::new(packed.decode_with::<STREAM>()?);
        if *decoded != *world {
            return Err(err("packed World roundtrip changed equality"));
        }
        Ok((packed, decoded))
    }
    fn decode(&self) -> Result<World, ModelError> {
        streaming::world(&self.0)
    }
    fn decode_with<const STREAM: bool>(&self) -> Result<World, ModelError> {
        if STREAM {
            self.decode()
        } else {
            self.decode_oracle()
        }
    }
    fn decode_oracle(&self) -> Result<World, ModelError> {
        serde_json::from_value(decode_value(&self.0)?).map_err(|e| err(&e.to_string()))
    }
}
// Memoization belongs to the model instance, NEVER to graph-retained State.
// The cache owns at most two exact byte keys and two decoded Worlds. Full byte
// equality is the lookup criterion; no digest can authorize a cache hit.
#[derive(Default)]
pub(super) struct PackedDownloads<const CACHE: bool = true, const STREAM: bool = true> {
    cache: RefCell<Vec<(PackedWorld, Rc<World>)>>,
    hits: Cell<usize>,
    misses: Cell<usize>,
    roundtrips: Cell<usize>,
    max_entries: Cell<usize>,
}
impl<const CACHE: bool, const STREAM: bool> PackedDownloads<CACHE, STREAM> {
    fn remember(&self, packed: &PackedWorld, world: Rc<World>) {
        if !CACHE {
            return;
        }
        let mut cache = self.cache.borrow_mut();
        if let Some(index) = cache.iter().position(|(key, _)| key == packed) {
            cache.remove(index);
        }
        if cache.len() == 2 {
            cache.remove(0);
        }
        cache.push((packed.clone(), world));
        self.max_entries
            .set(self.max_entries.get().max(cache.len()));
        assert!(cache.len() <= 2);
    }
    fn decoded(&self, packed: &PackedWorld) -> Result<Rc<World>, ModelError> {
        if CACHE {
            let mut cache = self.cache.borrow_mut();
            if let Some(index) = cache.iter().position(|(key, _)| key == packed) {
                let entry = cache.remove(index);
                let world = Rc::clone(&entry.1);
                cache.push(entry);
                self.hits.set(self.hits.get() + 1);
                return Ok(world);
            }
        }
        self.misses.set(self.misses.get() + 1);
        let world = Rc::new(packed.decode_with::<STREAM>()?);
        self.remember(packed, Rc::clone(&world));
        Ok(world)
    }
    fn encoded(&self, world: &World) -> Result<PackedWorld, ModelError> {
        // Independent binary decode + exact World comparison on EVERY successor,
        // including repeated/self-loop states. A cache hit never skips this proof.
        let (packed, decoded) = PackedWorld::encode_checked_with::<STREAM>(world)?;
        self.roundtrips.set(self.roundtrips.get() + 1);
        self.remember(&packed, decoded);
        Ok(packed)
    }
}
impl<const CACHE: bool, const STREAM: bool> Model for PackedDownloads<CACHE, STREAM> {
    type State = PackedWorld;
    type Input = Input;
    type Output = <Downloads as Model>::Output;
    fn metadata(&self) -> ModelMetadata {
        Downloads.metadata()
    }
    fn initial_state(&self) -> Result<Self::State, ModelError> {
        self.encoded(&Downloads.initial_state()?)
    }
    fn step(
        &self,
        w: &Self::State,
        input: &Input,
    ) -> Result<Transition<Self::State, Self::Output>, ModelError> {
        let before = self.decoded(w)?;
        let t = Downloads.step(&before, input)?;
        Ok(Transition {
            state: self.encoded(&t.state)?,
            outputs: t.outputs,
            disposition: t.disposition,
        })
    }
    fn check_state(&self, w: &Self::State) -> Result<Vec<Check>, ModelError> {
        {
            let decoded = self.decoded(w)?;
            Downloads.check_state(&decoded)
        }
    }
    fn check_transition(
        &self,
        w: &Self::State,
        input: &Input,
        t: &TransitionRef<'_, Self::State, Self::Output>,
    ) -> Result<Vec<Check>, ModelError> {
        let before = self.decoded(w)?;
        let after = self.decoded(t.state)?;
        Downloads.check_transition(
            &before,
            input,
            &TransitionRef {
                state: &after,
                outputs: t.outputs,
                disposition: t.disposition,
            },
        )
    }
}
impl<const CACHE: bool, const STREAM: bool> Enumerate for PackedDownloads<CACHE, STREAM> {
    fn inputs(&self, w: &Self::State) -> Result<Vec<Input>, ModelError> {
        {
            let decoded = self.decoded(w)?;
            Downloads.inputs(&decoded)
        }
    }
}

#[test]
fn packed_tree_all_tags_extremes_and_canonical_failures() {
    let cases = [
        Value::Null,
        Value::Bool(false),
        Value::Bool(true),
        Value::from(0u64),
        Value::from(u64::MAX),
        Value::from(i64::MIN),
        Value::from(-1),
        Value::String("view".into()),
        Value::String("".into()),
        Value::String("variable-λ\0".into()),
        serde_json::json!([null,true,1,"1",{"z":[],"a":false}]),
    ];
    for value in cases {
        let bytes = encode_value(&value).unwrap();
        assert_eq!(decode_value(&bytes).unwrap(), value);
    }
    assert!(encode_value(&serde_json::json!(1.5)).is_err());
    for bytes in [
        vec![],
        vec![2, 0],
        vec![1, 0, 0],
        vec![1, 3, 0x80, 0],
        vec![1, 9],
        vec![1, 5, 1, 0xff],
        vec![1, 8, 0],
        vec![1, 4, 0xff, 0x01],
        vec![1, 5, 4, b'v', b'i', b'e', b'w'],
    ] {
        assert!(decode_value(&bytes).is_err(), "accepted {bytes:?}");
    }
    let mut keys = vec![1, 7, 2];
    string("z", &mut keys);
    keys.push(0);
    string("a", &mut keys);
    keys.push(0);
    assert!(decode_value(&keys).is_err());
}

#[test]
fn packed_world_preserves_optional_strings_enums_and_nondefault_fields() {
    let mut w = Downloads.initial_state().unwrap();
    for i in [
        Input::Download,
        Input::Play(Position::Partial),
        Input::Play(Position::Complete),
        Input::SyncLost,
    ] {
        w = Downloads.step(&w, &i).unwrap().state;
    }
    w.view.session = Some("arbitrary session \0 λ".into());
    w.view.manual_watched = Some(true);
    w.base_epoch = Some(255);
    w.epoch = 254;
    w.restored_base = (u64::MAX, u64::MAX - 1);
    if let Some(pin) = &mut w.download {
        pin.source_revision = "not-a-token".into();
        pin.viewing_epoch = None;
    }
    if let Some(event) = w.log.first_mut() {
        event.scope.server_id = "other".into();
        event.device_time = Some(String::new());
        event.media.viewing_epoch = None;
        event.event_id = "independent-id".into();
    }
    for outcome in [Outcome::Applied, Outcome::HistoryOnly, Outcome::Rejected] {
        for code in [
            None,
            Some(Code::NotFound),
            Some(Code::InvalidEvent),
            Some(Code::InvalidPosition),
            Some(Code::TimelineChanged),
            Some(Code::ManualOverrideNewer),
            Some(Code::ViewingNewer),
        ] {
            for record in &mut w.recorded {
                record.outcome = outcome;
                record.code = code;
                record.event.device_time = "arbitrary-time".into();
            }
            assert_eq!(PackedWorld::encode(&w).unwrap().decode().unwrap(), w);
            assert_eq!(PackedWorld::encode(&w).unwrap().decode_oracle().unwrap(), w);
        }
    }
    for status in [
        Status::Playing,
        Status::Paused,
        Status::Ended,
        Status::Stopped,
        Status::Superseded,
    ] {
        if let Some(event) = &mut w.first {
            event.status = status;
        }
        for record in &mut w.recorded {
            record.event.status = status;
        }
        assert_eq!(PackedWorld::encode(&w).unwrap().decode().unwrap(), w);
        assert_eq!(PackedWorld::encode(&w).unwrap().decode_oracle().unwrap(), w);
    }
    // Vec capacity is not Eq state, and cannot affect retained bytes.
    let mut other = w.clone();
    other.log.reserve(100);
    other.recorded.reserve(100);
    assert_eq!(
        PackedWorld::encode(&w).unwrap(),
        PackedWorld::encode(&other).unwrap()
    );
}

#[test]
fn packed_and_original_exhaustive_prefix_edges_and_checks_are_identical() {
    use std::collections::{HashSet, VecDeque};
    let cached = PackedDownloads::<true>::default();
    let uncached = PackedDownloads::<false>::default();
    assert_eq!(
        cached.initial_state().unwrap(),
        uncached.initial_state().unwrap()
    );
    let initial = Downloads.initial_state().unwrap();
    let mut seen = HashSet::from([initial.clone()]);
    let mut queue = VecDeque::from([(initial, 0)]);
    let mut edges = 0usize;
    let mut packed_bytes = 0usize;
    let mut json_bytes = 0usize;
    while let Some((world, depth)) = queue.pop_front() {
        let packed = PackedWorld::encode(&world).unwrap();
        assert_eq!(packed.decode().unwrap(), packed.decode_oracle().unwrap());
        assert_eq!(
            packed,
            PackedWorld::encode_checked_with::<false>(&world).unwrap().0
        );
        packed_bytes += packed.0.len();
        json_bytes += serde_json::to_vec(&world).unwrap().len();
        assert_eq!(
            cached.check_state(&packed).unwrap(),
            Downloads.check_state(&world).unwrap()
        );
        assert_eq!(
            cached.check_state(&packed).unwrap(),
            uncached.check_state(&packed).unwrap()
        );
        assert_eq!(
            cached.inputs(&packed).unwrap(),
            uncached.inputs(&packed).unwrap()
        );
        let inputs = Downloads.inputs(&world).unwrap();
        assert_eq!(cached.inputs(&packed).unwrap(), inputs);
        assert_eq!(inputs.len(), 16);
        if depth == 6 {
            continue;
        }
        for input in inputs {
            let plain = Downloads.step(&world, &input).unwrap();
            let compact = cached.step(&packed, &input).unwrap();
            let baseline = uncached.step(&packed, &input).unwrap();
            edges += 1;
            assert_eq!(compact.state, baseline.state);
            assert_eq!(compact.outputs, baseline.outputs);
            assert_eq!(compact.disposition, baseline.disposition);
            assert_eq!(
                cached.check_state(&compact.state).unwrap(),
                Downloads.check_state(&plain.state).unwrap()
            );
            assert_eq!(
                cached.check_state(&compact.state).unwrap(),
                uncached.check_state(&baseline.state).unwrap()
            );
            assert_eq!(
                cached
                    .check_transition(&packed, &input, &compact.as_ref())
                    .unwrap(),
                uncached
                    .check_transition(&packed, &input, &baseline.as_ref())
                    .unwrap()
            );
            assert_eq!(
                compact.state.decode().unwrap(),
                compact.state.decode_oracle().unwrap()
            );
            assert_eq!(compact.state.decode().unwrap(), plain.state);
            assert_eq!(compact.outputs, plain.outputs);
            assert_eq!(compact.disposition, plain.disposition);
            assert_eq!(
                cached
                    .check_transition(&packed, &input, &compact.as_ref())
                    .unwrap(),
                Downloads
                    .check_transition(&world, &input, &plain.as_ref())
                    .unwrap()
            );
            if seen.insert(plain.state.clone()) {
                queue.push_back((plain.state, depth + 1));
            }
        }
    }
    assert!(
        packed_bytes < json_bytes / 2,
        "binary dictionary representation must materially shrink encoded states"
    );
    println!(
        "Packed prefix: {} exact states, {edges} exact edges; World inline={} PackedWorld inline={}; aggregate packed={packed_bytes} JSON={json_bytes}",
        seen.len(),
        std::mem::size_of::<World>(),
        std::mem::size_of::<PackedWorld>()
    );
    assert_eq!(cached.roundtrips.get(), edges + 1);
    assert_eq!(uncached.roundtrips.get(), edges + 1);
    assert_eq!(cached.max_entries.get(), 2);
    assert_eq!(uncached.max_entries.get(), 0);
    let config = stateless::explore::SearchConfig {
        max_states: 12_000_000,
        max_transitions: 200_000_000,
        max_depth: 6,
    };
    let started = std::time::Instant::now();
    let plain = stateless::explore::enumerate(&Downloads, config.clone()).unwrap();
    let plain_elapsed = started.elapsed();
    let baseline = PackedDownloads::<false>::default();
    let started = std::time::Instant::now();
    let unpacked_cache = stateless::explore::enumerate(&baseline, config.clone()).unwrap();
    let uncached_elapsed = started.elapsed();
    let oracle = PackedDownloads::<true, false>::default();
    let started = std::time::Instant::now();
    let oracle_report = stateless::explore::enumerate(&oracle, config.clone()).unwrap();
    let oracle_elapsed = started.elapsed();
    let model = PackedDownloads::<true>::default();
    let started = std::time::Instant::now();
    let compact = stateless::explore::enumerate(&model, config).unwrap();
    let cached_elapsed = started.elapsed();
    assert_eq!(
        (
            unpacked_cache.states,
            unpacked_cache.transitions,
            unpacked_cache.max_depth_reached,
            unpacked_cache.skipped_checks,
            unpacked_cache.termination
        ),
        (
            compact.states,
            compact.transitions,
            compact.max_depth_reached,
            compact.skipped_checks,
            compact.termination
        )
    );
    assert_eq!(unpacked_cache.failure, compact.failure);
    assert_eq!(model.roundtrips.get(), baseline.roundtrips.get());
    assert_eq!(
        u64::try_from(model.roundtrips.get()).unwrap(),
        compact.transitions + 1
    );
    assert_eq!(model.max_entries.get(), 2);
    assert_eq!(oracle.max_entries.get(), 2);
    assert_eq!(oracle.roundtrips.get(), model.roundtrips.get());
    assert_eq!(
        (
            oracle_report.states,
            oracle_report.transitions,
            oracle_report.max_depth_reached,
            oracle_report.skipped_checks,
            oracle_report.termination,
            &oracle_report.failure
        ),
        (
            compact.states,
            compact.transitions,
            compact.max_depth_reached,
            compact.skipped_checks,
            compact.termination,
            &compact.failure
        )
    );
    println!(
        "Decoder prefix timings: cached Value oracle={oracle_elapsed:?} cached streaming={cached_elapsed:?}; exact independent roundtrips={}",
        model.roundtrips.get()
    );
    assert!(model.misses.get() < baseline.misses.get());
    println!(
        "Prefix timings: original={plain_elapsed:?} uncached={uncached_elapsed:?} cached={cached_elapsed:?}; uncached callback decodes={} cached callback decodes={} hits={} independent roundtrips={} max entries={}",
        baseline.misses.get(),
        model.misses.get(),
        model.hits.get(),
        model.roundtrips.get(),
        model.max_entries.get()
    );
    assert_eq!(
        (
            plain.states,
            plain.transitions,
            plain.max_depth_reached,
            plain.skipped_checks,
            plain.termination
        ),
        (
            compact.states,
            compact.transitions,
            compact.max_depth_reached,
            compact.skipped_checks,
            compact.termination
        )
    );
    assert_eq!(plain.failure, compact.failure);
    let corpus: Vec<_> = seen
        .iter()
        .map(|world| PackedWorld::encode(world).unwrap())
        .collect();
    for round in 0..3 {
        let started = std::time::Instant::now();
        for packed in &corpus {
            std::hint::black_box(packed.decode_oracle().unwrap());
        }
        let old = started.elapsed();
        let started = std::time::Instant::now();
        for packed in &corpus {
            std::hint::black_box(packed.decode().unwrap());
        }
        println!(
            "Decoder-only exact corpus round {round}: states={} Value={old:?} streaming={:?}",
            corpus.len(),
            started.elapsed()
        );
    }
}

#[test]
fn packed_cache_exact_keys_eviction_and_invalid_bytes() {
    let model = PackedDownloads::<true>::default();
    let a = Downloads.initial_state().unwrap();
    let mut b = a.clone();
    b.next_sequence += 1;
    let mut c = a.clone();
    c.next_sequence += 2;
    let pa = PackedWorld::encode(&a).unwrap();
    let pb = PackedWorld::encode(&b).unwrap();
    let pc = PackedWorld::encode(&c).unwrap();
    let first = model.decoded(&pa).unwrap();
    let repeated = model.decoded(&pa.clone()).unwrap();
    assert!(Rc::ptr_eq(&first, &repeated));
    assert_eq!(model.misses.get(), 1);
    assert_eq!(model.hits.get(), 1);
    drop((first, repeated));
    assert_eq!(*model.decoded(&pb).unwrap(), b);
    // Hit A makes B oldest; inserting C must evict B, not A.
    assert_eq!(*model.decoded(&pa).unwrap(), a);
    assert_eq!(*model.decoded(&pc).unwrap(), c);
    assert_eq!(
        model
            .cache
            .borrow()
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>(),
        vec![pa.clone(), pc]
    );
    let before = model.misses.get();
    assert_eq!(*model.decoded(&pb).unwrap(), b);
    assert_eq!(model.misses.get(), before + 1);
    assert_eq!(model.max_entries.get(), 2);
    let retained = model
        .cache
        .borrow()
        .iter()
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for bytes in [vec![], vec![1, 3, 0x80, 0], vec![1, 0, 0], {
        let mut bytes = pa.0.to_vec();
        bytes.push(0);
        bytes
    }] {
        assert!(
            model
                .decoded(&PackedWorld(bytes.into_boxed_slice()))
                .is_err()
        );
        assert_eq!(
            model
                .cache
                .borrow()
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>(),
            retained
        );
    }
    // Even a warm identical successor gets an independent decode roundtrip.
    let before = model.roundtrips.get();
    assert_eq!(model.encoded(&a).unwrap(), pa);
    assert_eq!(model.encoded(&a).unwrap(), pa);
    assert_eq!(model.roundtrips.get(), before + 2);
    // Callback ordering and eviction cannot affect the delegated verdicts.
    model.cache.borrow_mut().clear();
    assert_eq!(
        model.check_state(&pb).unwrap(),
        Downloads.check_state(&b).unwrap()
    );
    assert_eq!(model.inputs(&pa).unwrap(), Downloads.inputs(&a).unwrap());
    assert_eq!(
        model.check_state(&pa).unwrap(),
        Downloads.check_state(&a).unwrap()
    );
}

#[test]
fn streaming_decoder_matches_oracle_malformed_corpus_and_enum_shapes() {
    fn tree(bytes: &[u8]) {
        let old = decode_value(bytes);
        let new = streaming::decode::<Value>(bytes);
        assert_eq!(old.is_ok(), new.is_ok(), "tree verdict bytes={bytes:?}");
        if let (Ok(old), Ok(new)) = (old, new) {
            assert_eq!(old, new);
        }
    }
    fn world(bytes: &[u8]) {
        let packed = PackedWorld(bytes.into());
        let old = packed.decode_oracle();
        let new = packed.decode();
        assert_eq!(old.is_ok(), new.is_ok(), "World verdict bytes={bytes:?}");
        if let (Ok(old), Ok(new)) = (old, new) {
            assert_eq!(old, new);
        }
    }
    let initial = Downloads.initial_state().unwrap();
    let original = serde_json::to_value(&initial).unwrap();
    let mut worlds = vec![original.clone()];
    for value in [
        Value::Null,
        Value::Bool(true),
        Value::from(-1),
        Value::from(0),
        Value::from(255),
        Value::from(256),
        Value::from(u64::MAX),
        Value::String("invalid".into()),
    ] {
        let mut changed = original.clone();
        changed["base_epoch"] = value;
        worlds.push(changed);
    }
    let mut unknown = original.clone();
    unknown["zz_unknown"] = serde_json::json!({"a":[null,true,{"nested":"variable-λ"}]});
    worlds.push(unknown);
    let mut missing = original.clone();
    missing.as_object_mut().unwrap().remove("view");
    worlds.push(missing);
    let mut corpus = vec![
        Value::Null,
        Value::Bool(false),
        Value::Bool(true),
        Value::from(u64::MAX),
        Value::from(i64::MIN),
        Value::from(-1),
        Value::String("view".into()),
        Value::String("variable-λ\0".into()),
        serde_json::json!({"a":[],"b":[1,true,null]}),
    ];
    corpus.extend(worlds);
    let mutations = [0, 1, 3, 4, 5, 6, 7, 8, 127, 128, 255];
    let mut checked = 0usize;
    for value in corpus {
        let bytes = encode_value(&value).unwrap();
        tree(&bytes);
        world(&bytes);
        for end in 0..bytes.len() {
            tree(&bytes[..end]);
            world(&bytes[..end]);
            checked += 1;
        }
        for index in 0..bytes.len() {
            for tag in mutations {
                let mut altered = bytes.clone();
                altered[index] = tag;
                tree(&altered);
                world(&altered);
                checked += 1;
            }
        }
        let mut trailing = bytes;
        trailing.push(0);
        tree(&trailing);
        world(&trailing);
    }
    // Explicit noncanonical strings/varints, invalid signed zero/overflow,
    // duplicate/reversed keys, non-string key and nested ignored-value checks.
    let mut special = vec![
        vec![1, 3, 128, 0],
        vec![1, 8, 0],
        vec![1, 5, 4, b'v', b'i', b'e', b'w'],
        vec![1, 5, 1, 255],
        vec![1, 7, 1, 3, 0, 0],
        vec![1, 7, 2, 4, 0, 0, 4, 0, 0],
        vec![1, 7, 2, 4, 0, 0, 4, 7, 0],
        vec![1, 4, 255, 127],
    ];
    for depth in [127, 128, 129, 130] {
        let mut bytes = vec![1];
        for _ in 0..depth {
            bytes.extend([6, 1]);
        }
        bytes.push(0);
        special.push(bytes);
    }
    for bytes in special {
        tree(&bytes);
        world(&bytes);
        checked += 1;
    }
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    enum Variants {
        Unit,
        New(String),
        Tuple(u64, bool),
        Struct { count: u8, text: String },
    }
    for value in [
        Variants::Unit,
        Variants::New("λ".into()),
        Variants::Tuple(u64::MAX, true),
        Variants::Struct {
            count: u8::MAX,
            text: "view".into(),
        },
    ] {
        let json = serde_json::to_value(&value).unwrap();
        let bytes = encode_value(&json).unwrap();
        assert_eq!(streaming::decode::<Variants>(&bytes).unwrap(), value);
        assert_eq!(
            serde_json::from_value::<Variants>(decode_value(&bytes).unwrap()).unwrap(),
            value
        );
    }
    for count in [
        serde_json::json!(-1),
        serde_json::json!(0),
        serde_json::json!(255),
        serde_json::json!(256),
        serde_json::json!(u64::MAX),
    ] {
        let json = serde_json::json!({"Struct":{"count":count,"text":"view"}});
        let bytes = encode_value(&json).unwrap();
        let old = serde_json::from_value::<Variants>(decode_value(&bytes).unwrap());
        let new = streaming::decode::<Variants>(&bytes);
        assert_eq!(old.is_ok(), new.is_ok());
        if let (Ok(old), Ok(new)) = (old, new) {
            assert_eq!(old, new);
        }
    }
    for number in [
        Value::from(i64::MIN),
        Value::from(-1),
        Value::from(0),
        Value::from(i64::MAX),
        Value::from(u64::MAX),
    ] {
        let mut value = original.clone();
        value["timeline_revision"] = number;
        world(&encode_value(&value).unwrap());
    }
    // World ignores this unknown field, but must not ignore its canonical
    // encoding, malformed nested varint or original depth bound.
    for depth in [126, 127, 128, 129] {
        let mut nested = Value::Null;
        for _ in 0..depth {
            nested = Value::Array(vec![nested]);
        }
        let mut value = original.clone();
        value["zz_unknown"] = nested;
        let mut bytes = encode_value(&value).unwrap();
        tree(&bytes);
        world(&bytes);
        assert_eq!(bytes.pop(), Some(0));
        bytes.extend([3, 128, 0]);
        assert!(decode_value(&bytes).is_err());
        tree(&bytes);
        world(&bytes);
    }
    let external_unit = encode_value(&serde_json::json!({"Unit":null})).unwrap();
    assert_eq!(
        streaming::decode::<Variants>(&external_unit).unwrap(),
        Variants::Unit
    );
    println!(
        "Streaming decode parity: {checked} deterministic truncation/mutation cases, tree and World verdicts, all external enum shapes"
    );
}
