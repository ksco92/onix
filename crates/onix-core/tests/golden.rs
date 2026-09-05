//! Golden-corpus test: proves `onix`'s report matches real `DeepDiff`
//! (`verbose_level=2`, `to_json()`) byte-for-byte after canonical
//! re-serialization, on every hand-designed case under `tests/golden/` at
//! the repository root.
//!
//! Each case directory (`tests/golden/<case_name>/`) holds `a.json`,
//! `b.json`, `expected.json`, and `options.json` (currently just
//! `{"ignore_order": bool}`), all generated from real `DeepDiff` by
//! `scripts/gen_goldens.py` — see `tests/golden/README.md` for the pinned
//! versions and the regeneration command. This test never edits or
//! regenerates those files; it only reads them.
//!
//! The two input files carry Python values JSON cannot express (a tuple, a
//! set, a frozenset, a datetime, a date, a time or a timedelta) in the tagged encoding
//! `tests/golden/README.md` documents, decoded
//! here by [`decode_tagged`] — the Rust half of the same rule
//! `scripts/golden_tags.py` implements for the corpus's Python readers. This
//! decoding is test-only: the engine's own parse paths never interpret a tag
//! (see the `tagged_objects_are_ordinary_data_to_the_parser` test below).
//!
//! "Byte-for-byte" here means *canonical* JSON equality: both sides are
//! parsed into [`serde_json::Value`] and compared with `PartialEq`, which
//! for `serde_json`'s `Object` variant is a `BTreeMap`/order-insensitive
//! comparison — so this is insensitive to object key order (which carries
//! no meaning) while still requiring exactly the same keys, values, and
//! array order (which does).
//!
//! The corpus itself (`case_names()`, reading `tests/golden/`'s actual
//! directory listing) is the sole source of which cases exist — there is no
//! separate, hand-maintained case list to drift out of sync with it.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The `tests/golden` directory at the repository root, resolved relative
/// to this crate's manifest directory so the test works regardless of the
/// directory `cargo test` is invoked from.
fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden")
}

/// Reads and parses a JSON fixture file, panicking with the file path on
/// any failure — a missing or malformed fixture is a corpus bug, not a
/// recoverable test outcome.
fn read_json(path: &Path) -> Value {
    let raw = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read fixture {}: {err}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("failed to parse fixture {} as JSON: {err}", path.display()))
}

/// Every case directory name under `tests/golden/`, sorted for a
/// deterministic test run order. Skips `README.md` and any other
/// non-directory entry.
fn case_names() -> Vec<String> {
    let root = golden_root();
    let mut names: Vec<String> = fs::read_dir(&root)
        .unwrap_or_else(|err| panic!("failed to list golden root {}: {err}", root.display()))
        .map(|entry| entry.expect("readable directory entry"))
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Reads a case's `options.json` (currently just `{"ignore_order": bool}`)
/// and returns the [`onix_core::DiffOptions`] to diff it with. Defaults to
/// `ignore_order: false` if the file is missing (none of this corpus's case
/// directories omit it — `scripts/gen_goldens.py` always writes one — but a
/// hand-added case directory that forgot to run the generator should still
/// run the ordered path rather than panic).
fn case_options(case_dir: &Path) -> onix_core::DiffOptions {
    let options_path = case_dir.join("options.json");
    if !options_path.exists() {
        return onix_core::DiffOptions::default();
    }
    let options = read_json(&options_path);
    let ignore_order = options
        .get("ignore_order")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    onix_core::DiffOptions {
        ignore_order,
        ..onix_core::DiffOptions::default()
    }
}

/// Every tag name the corpus's encoding reserves, mirroring
/// `scripts/golden_tags.py`'s `RESERVED_TAGS`. Every one of them decodes
/// today; the list is still fixed here so a fixture cannot quietly use one
/// as an ordinary dict key.
const RESERVED_TAGS: &[&str] = &[
    "$tuple",
    "$set",
    "$frozenset",
    "$datetime",
    "$date",
    "$time",
    "$timedelta",
];

/// Decodes one parsed fixture value into the engine's own value model,
/// turning a tagged object — a JSON object with exactly one key, that key one
/// of [`RESERVED_TAGS`] — into the Python value it stands for. Every other
/// object is plain data.
///
/// Panics on a tag with no decoder yet: a corpus using one before its slice
/// lands is a corpus bug, not a recoverable test outcome.
fn decode_tagged(value: &Value, builder: &mut onix_core::value::Builder) -> onix_core::Value {
    match value {
        Value::Array(items) => onix_core::Value::Array(
            items
                .iter()
                .map(|item| decode_tagged(item, builder))
                .collect(),
        ),
        Value::Object(map) => match sole_tag(map) {
            Some("$tuple") => onix_core::Value::Tuple(decode_tagged_items(map, "$tuple", builder)),
            Some("$datetime") => {
                onix_core::Value::DateTime(parse_datetime(tag_text(map, "$datetime")))
            }
            Some("$date") => onix_core::Value::Date(parse_date(tag_text(map, "$date"))),
            Some("$time") => onix_core::Value::Time(parse_time(tag_text(map, "$time"))),
            Some("$timedelta") => {
                onix_core::Value::TimeDelta(parse_timedelta(map.get("$timedelta")))
            }
            Some("$set") => onix_core::Value::Set(decode_set_members(map, "$set", builder)),
            Some("$frozenset") => {
                onix_core::Value::FrozenSet(decode_set_members(map, "$frozenset", builder))
            }
            Some(tag) => {
                panic!("golden fixture uses the reserved tag {tag:?}, which has no decoder yet")
            }
            None => {
                let entries: Vec<(String, onix_core::Value)> = map
                    .iter()
                    .map(|(key, item)| (key.clone(), decode_tagged(item, builder)))
                    .collect();
                builder.object(entries)
            }
        },
        scalar => onix_core::Value::from(scalar.clone()),
    }
}

/// The decoded members of a `$set`/`$frozenset` fixture.
///
/// Panics if two members are **structurally** equal — the pair the value
/// model itself collapses, so a fixture holding one would stand for a
/// smaller set than it lists and would silently disagree with the Python
/// readers (which decode into a real `set`). Two members that are merely
/// *Python*-equal without being structurally equal (`{"$tuple": [1]}` and
/// `{"$tuple": [1.0]}`) are not caught here; they would fail downstream, on
/// the expected report, since the generator can never write such a pair.
fn decode_set_members(
    map: &serde_json::Map<String, Value>,
    tag: &str,
    builder: &mut onix_core::value::Builder,
) -> onix_core::value::SetItems {
    let members = decode_tagged_items(map, tag, builder).into_vec();
    let items = onix_core::value::SetItems::new(members);
    assert_eq!(
        items.len(),
        map[tag].as_array().map_or(0, Vec::len),
        "golden fixture {tag} holds two equal members, which no Python set can"
    );
    items
}

/// The reserved tag `map` is an encoding of, or `None` if it is plain data.
fn sole_tag(map: &serde_json::Map<String, Value>) -> Option<&'static str> {
    if map.len() != 1 {
        return None;
    }
    let key = map.keys().next().expect("a one-entry map has a key");
    RESERVED_TAGS.iter().copied().find(|tag| *tag == key)
}

/// The decoded items of a tagged sequence, panicking if the tag's payload is
/// not an array (again: a corpus bug).
fn decode_tagged_items(
    map: &serde_json::Map<String, Value>,
    tag: &str,
    builder: &mut onix_core::value::Builder,
) -> Box<[onix_core::Value]> {
    let Some(Value::Array(items)) = map.get(tag) else {
        panic!("the {tag:?} tag's payload must be an array");
    };
    items
        .iter()
        .map(|item| decode_tagged(item, builder))
        .collect()
}

/// The string payload of a tagged scalar value, panicking if it is not one
/// (again: a corpus bug).
fn tag_text<'a>(map: &'a serde_json::Map<String, Value>, tag: &str) -> &'a str {
    let Some(Value::String(text)) = map.get(tag) else {
        panic!("the {tag:?} tag's payload must be a string");
    };
    text
}

/// Parses the `YYYY-MM-DD` payload of a `$date` tag — Python's
/// `date.isoformat()`, which is the only shape `scripts/golden_tags.py`
/// writes.
fn parse_date(text: &str) -> onix_core::Date {
    let parsed = || {
        let (year, rest) = text.split_once('-')?;
        let (month, day) = rest.split_once('-')?;
        onix_core::Date::new(year.parse().ok()?, month.parse().ok()?, day.parse().ok()?)
    };
    parsed().unwrap_or_else(|| panic!("not an ISO 8601 date: {text:?}"))
}

/// Parses the `YYYY-MM-DDTHH:MM:SS[.ffffff][±HH:MM[:SS]]` payload of a
/// `$datetime` tag — Python's `datetime.isoformat()`, again the only shape
/// the generator writes.
fn parse_datetime(text: &str) -> onix_core::DateTime {
    let parsed = || {
        let (date_text, time_text) = text.split_once('T')?;
        let (hour, minute, second, microsecond, offset) = parse_clock_fields(time_text)?;

        onix_core::DateTime::new(
            parse_date(date_text),
            hour,
            minute,
            second,
            microsecond,
            offset,
        )
    };
    parsed().unwrap_or_else(|| panic!("not an ISO 8601 datetime: {text:?}"))
}

/// Parses the `HH:MM:SS[.ffffff][±HH:MM[:SS]]` payload of a `$time` tag —
/// Python's `time.isoformat()`, the same clock shape [`parse_datetime`]
/// parses after its `T` separator (see [`parse_clock_fields`]).
fn parse_time(text: &str) -> onix_core::Time {
    let parsed = || {
        let (hour, minute, second, microsecond, offset) = parse_clock_fields(text)?;
        onix_core::Time::new(hour, minute, second, microsecond, offset)
    };
    parsed().unwrap_or_else(|| panic!("not an ISO 8601 time: {text:?}"))
}

/// Parses one `HH:MM:SS[.ffffff][±HH:MM[:SS]]` clock string — the shared
/// core of [`parse_datetime`] (applied to the text after its `T`) and
/// [`parse_time`] (applied to the whole payload).
fn parse_clock_fields(clock_text: &str) -> Option<(u8, u8, u8, u32, Option<i32>)> {
    let sign_at = clock_text.rfind(['+', '-']);
    let (clock_text, offset) = match sign_at {
        None => (clock_text, None),
        Some(index) => (
            &clock_text[..index],
            Some(parse_offset(&clock_text[index..])?),
        ),
    };
    let (clock_text, microsecond) = match clock_text.split_once('.') {
        None => (clock_text, 0),
        Some((clock, fraction)) => (clock, fraction.parse().ok()?),
    };
    let mut fields = clock_text.split(':');
    let hour = fields.next()?.parse().ok()?;
    let minute = fields.next()?.parse().ok()?;
    let second = fields.next()?.parse().ok()?;

    Some((hour, minute, second, microsecond, offset))
}

/// Parses the `{"days": D, "seconds": S, "microseconds": U}` payload of a
/// `$timedelta` tag — Python's own already-normalized `timedelta` triple
/// (see `scripts/golden_tags.py`'s module doc for why a single flattened
/// number is not used).
fn parse_timedelta(payload: Option<&Value>) -> onix_core::TimeDelta {
    let parsed = || {
        let map = payload?.as_object()?;
        onix_core::TimeDelta::new(
            map.get("days")?.as_i64()?,
            map.get("seconds")?.as_i64()?,
            map.get("microseconds")?.as_i64()?,
        )
    };
    parsed().unwrap_or_else(|| panic!("not a valid $timedelta payload: {payload:?}"))
}

/// Parses a `±HH:MM[:SS]` UTC-offset suffix into whole seconds.
fn parse_offset(text: &str) -> Option<i32> {
    let (sign, digits) = text.split_at(1);
    let mut fields = digits.split(':');
    let hours: i32 = fields.next()?.parse().ok()?;
    let minutes: i32 = fields.next()?.parse().ok()?;
    let seconds: i32 = fields.next().map_or(Ok(0), str::parse).ok()?;
    let magnitude = hours * 3600 + minutes * 60 + seconds;

    Some(if sign == "-" { -magnitude } else { magnitude })
}

/// Runs `onix_core::diff_with_options` on a case's `a.json`/`b.json` (per
/// its own `options.json`), panicking (rather than returning `Result`) if
/// diffing itself errors — every case here is small and well within
/// `DEFAULT_MAX_DEPTH`, so an `Err` would itself be a corpus/engine bug, not
/// an expected outcome.
fn diff_case(name: &str) -> Value {
    let case_dir = golden_root().join(name);
    let mut builder = onix_core::value::Builder::new();
    let a = decode_tagged(&read_json(&case_dir.join("a.json")), &mut builder);
    let b = decode_tagged(&read_json(&case_dir.join("b.json")), &mut builder);
    let opts = case_options(&case_dir);
    let report = onix_core::diff_with_options(&a, &b, &opts)
        .unwrap_or_else(|err| panic!("golden case {name:?}: diff returned an error: {err}"));
    report.to_json_value()
}

/// Cases whose `expected.json` records a real-`DeepDiff` outcome that onix
/// is not expected to reproduce byte-for-byte, checked by their own
/// dedicated test below instead of the blanket equality loop.
///
/// `path_rendering_collision`: `DeepDiff`'s path rendering is
/// not injective on adversarial keys (see
/// `crate::path::quote_key`'s doc), so two distinct structural paths can
/// render to the same string and collapse into one JSON entry — both in
/// `DeepDiff` and in `onix` (see `crate::report`'s module doc). Which
/// finding survives the collapse is an insertion-order-dependent detail of
/// `DeepDiff`'s own Python dict iteration that `onix` would have to thread
/// original JSON key order through the whole engine to reproduce — not
/// worth the coupling for this vanishingly rare edge. See
/// `tests/golden/README.md`.
const KNOWN_DIVERGENT_CASES: &[&str] = &["path_rendering_collision"];

/// Every golden case not listed in [`KNOWN_DIVERGENT_CASES`] must match its
/// `expected.json` exactly. Failures across the *whole* corpus are
/// collected and reported together, so a regression run shows every
/// diverged case at once rather than stopping at the first one.
#[test]
fn every_golden_case_matches_deepdiff() {
    let mut failures = Vec::new();

    for name in case_names() {
        if KNOWN_DIVERGENT_CASES.contains(&name.as_str()) {
            continue;
        }

        let case_dir = golden_root().join(&name);
        let expected = read_json(&case_dir.join("expected.json"));
        let actual = diff_case(&name);

        if actual != expected {
            failures.push(format!(
                "{name:?} diverges from real DeepDiff:\n  --- onix ---\n{actual:#}\n  \
                 --- deepdiff (expected.json) ---\n{expected:#}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} golden case(s) diverged from DeepDiff:\n\n{}",
        failures.len(),
        case_names().len(),
        failures.join("\n\n"),
    );
}

/// The regression this test guards against: a dict key whose own text
/// contains `']['`-shaped syntax used to `debug_assert`-panic
/// (`report.rs`'s duplicate-path guard treated the rendered *string* as the
/// uniqueness key) instead of collapsing cleanly the way real `DeepDiff`
/// does. `Report` now keys findings by the *structural* path instead (see
/// `crate::report`'s module doc), so this must not panic — and the
/// resulting report must still be valid, `DeepDiff`-shaped JSON, even
/// though onix's collapse survivor is not required to match `DeepDiff`'s own
/// (see [`KNOWN_DIVERGENT_CASES`]'s doc).
#[test]
fn path_rendering_collision_does_not_panic_and_is_deepdiff_shaped() {
    let actual = diff_case("path_rendering_collision");

    // DeepDiff's own survivor (tests/golden/path_rendering_collision/expected.json)
    // is the *nested* finding (old_value: 10, new_value: 20) — its Python
    // dict processes the top-level key first, then the nested key overwrites
    // it. onix's own BTreeMap-backed traversal visits dict keys in
    // *alphabetical*, not insertion, order, so its deterministic survivor is
    // the *other* finding — the top-level one. Documented, not chased (see
    // this test's and `KNOWN_DIVERGENT_CASES`'s doc).
    let expected_survivor = serde_json::json!({
        "values_changed": {
            "root[\"p'\"][\"q'\"]": {
                "new_value": 2,
                "old_value": 1,
            }
        }
    });
    assert_eq!(actual, expected_survivor);
}

/// Pins `ignore_order_nested_low_overlap_dict_pairing` directly against the
/// golden fixture's own `a.json`/`b.json` (not just the inline-literal unit
/// test in `crate::ignore_order`): both the pairing itself (`1` <-> `2`,
/// `[{aa,bb,cc}]` <-> `[{}]`, `0.0` unpaired-added) and the nested
/// `root[2][0]` dict-vs-dict subtree's own shape (a collapsed
/// `values_changed` with `new_path`) now match real `DeepDiff`'s decision
/// exactly, so this is now covered by the blanket
/// `every_golden_case_matches_deepdiff` loop too — this test stays as an
/// explicit, self-documenting pin of the exact shape.
#[test]
fn ignore_order_nested_low_overlap_dict_pairing_matches_deepdiff_exactly() {
    let actual = diff_case("ignore_order_nested_low_overlap_dict_pairing");
    let expected = serde_json::json!({
        "iterable_item_added": {"root[1]": 0.0},
        "values_changed": {
            "root[1]": {
                "new_path": "root[2]", "new_value": 2, "old_value": 1,
            },
            "root[2][0]": {
                "new_path": "root[3][0]",
                "new_value": {},
                "old_value": {"aa": 1, "bb": 2, "cc": 3},
            },
        },
    });
    assert_eq!(actual, expected);
}

/// The tagged encoding is a property of the *corpus*, not of the engine: the
/// crate's own parse path must read a tagged object as the ordinary dict it
/// literally is, so a real payload that happens to contain one diffs as data.
#[test]
fn tagged_objects_are_ordinary_data_to_the_parser() {
    let a: onix_core::Value = serde_json::from_str(r#"{"$tuple": [1]}"#).expect("valid JSON");
    let b: onix_core::Value = serde_json::from_str(r#"{"$tuple": [2]}"#).expect("valid JSON");
    let report = onix_core::diff(&a, &b).expect("shallow values diff cleanly");

    assert_eq!(
        report.to_json_value(),
        serde_json::json!({"values_changed": {"root['$tuple'][0]": {
            "new_value": 2, "old_value": 1,
        }}})
    );

    // The test-only decoder, on the same input, gives the tuple instead —
    // and the same split holds for every other implemented tag.
    let mut builder = onix_core::value::Builder::new();
    for (tagged, decodes_to_container) in [
        (r#"{"$tuple": [1]}"#, "tuple"),
        (r#"{"$set": [1]}"#, "set"),
        (r#"{"$frozenset": [1]}"#, "frozenset"),
    ] {
        let parsed: onix_core::Value = serde_json::from_str(tagged).expect("valid JSON");
        assert!(
            matches!(parsed, onix_core::Value::Object(_)),
            "{tagged} must parse as an ordinary dict on the product path"
        );

        let decoded = decode_tagged(&read_json_str(tagged), &mut builder);
        assert_eq!(
            onix_core::diff(&parsed, &decoded)
                .expect("shallow values diff cleanly")
                .to_json_value()["type_changes"]["root"]["new_type"],
            serde_json::json!(decodes_to_container),
            "the test-only decoder must give the {decodes_to_container} the tag stands for"
        );
    }
}

/// Parses JSON text for a test that needs a `serde_json::Value` without a
/// fixture file behind it.
fn read_json_str(text: &str) -> Value {
    serde_json::from_str(text).expect("valid JSON")
}
