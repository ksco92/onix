//! Diffs every `tests/golden/<case>/` with onix and compares with `expected.json`
//! as canonical JSON (key order ignored). Layout and tags: `tests/golden/README.md`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The `tests/golden` directory at the repository root, resolved relative
/// to this crate's manifest directory so the test works regardless of the
/// directory `cargo test` is invoked from.
fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden")
}

/// Reads and parses a JSON fixture file, panicking with the file path on failure.
fn read_json(path: &Path) -> Value {
    let raw = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read fixture {}: {err}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("failed to parse fixture {} as JSON: {err}", path.display()))
}

/// Rewrites every `{"$bigint": "<digits>"}` tag in an `expected.json` report to the
/// nearest `f64`, the resolution `Report::to_json_value` renders a big integer at.
fn collapse_bigint_tags(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(digits)) = map.get("$bigint")
                && map.len() == 1
            {
                let as_float: f64 = digits
                    .parse()
                    .expect("a $bigint tag carries a decimal integer string");
                return serde_json::Number::from_f64(as_float).map_or(Value::Null, Value::Number);
            }
            Value::Object(
                map.into_iter()
                    .map(|(key, item)| (key, collapse_bigint_tags(item)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(collapse_bigint_tags).collect()),
        other => other,
    }
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

/// A case's `DiffOptions`; a missing `options.json` means defaults.
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

/// The corpus's reserved tags; mirrors `scripts/golden_tags.py`'s `RESERVED_TAGS`.
const RESERVED_TAGS: &[&str] = &[
    "$tuple",
    "$set",
    "$frozenset",
    "$datetime",
    "$date",
    "$time",
    "$timedelta",
    "$dict",
    "$bigint",
    "$object",
];

/// Decodes one parsed fixture value into the engine's own value model,
/// turning a tagged object — a JSON object with exactly one key, that key one
/// of [`RESERVED_TAGS`] — into the Python value it stands for. Every other
/// object is plain data.
///
/// Panics on a reserved tag without a decoder.
fn decode_tagged(value: &Value, builder: &mut onix_core::value::Builder) -> onix_core::Value {
    match value {
        Value::Array(items) => onix_core::Value::Array(
            items
                .iter()
                .map(|item| decode_tagged(item, builder))
                .collect::<Box<[_]>>()
                .into(),
        ),
        Value::Object(map) => match sole_tag(map) {
            Some("$tuple") => {
                onix_core::Value::Tuple(decode_tagged_items(map, "$tuple", builder).into())
            }
            Some("$bigint") => onix_core::Value::Number(onix_core::Number::from_bigint(
                tag_text(map, "$bigint")
                    .parse::<num_bigint::BigInt>()
                    .expect("a $bigint tag carries a decimal integer string"),
            )),
            Some("$datetime") => {
                onix_core::Value::DateTime(parse_datetime(tag_text(map, "$datetime")).into())
            }
            Some("$date") => onix_core::Value::Date(parse_date(tag_text(map, "$date")).into()),
            Some("$time") => onix_core::Value::Time(parse_time(tag_text(map, "$time")).into()),
            Some("$timedelta") => {
                onix_core::Value::TimeDelta(parse_timedelta(map.get("$timedelta")).into())
            }
            Some("$set") => onix_core::Value::Set(decode_set_members(map, "$set", builder)),
            Some("$frozenset") => {
                onix_core::Value::FrozenSet(decode_set_members(map, "$frozenset", builder))
            }
            Some("$dict") => decode_tagged_dict(map, builder),
            Some("$object") => decode_tagged_object(map, builder),
            Some(tag) => {
                panic!("golden fixture uses the reserved tag {tag:?}, which has no decoder")
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

/// Decodes a `$dict` fixture: a JSON object cannot represent a non-`str`
/// key, so this tag's payload is a list of `[key, value]` pairs rather than the
/// object shape every other tag's plain-data fallback uses.
fn decode_tagged_dict(
    map: &serde_json::Map<String, Value>,
    builder: &mut onix_core::value::Builder,
) -> onix_core::Value {
    let Some(Value::Array(pairs)) = map.get("$dict") else {
        panic!("the \"$dict\" tag's payload must be an array");
    };

    let entries: Vec<(onix_core::value::ObjectKey, onix_core::Value)> = pairs
        .iter()
        .map(|pair| {
            let Value::Array(kv) = pair else {
                panic!("each \"$dict\" entry must be a [key, value] pair");
            };
            let [key, item] = kv.as_slice() else {
                panic!("each \"$dict\" entry must be a [key, value] pair");
            };

            let decoded_key = decode_tagged(key, builder);
            let key = match &decoded_key {
                // Fixture JSON is valid UTF-8, so a `$dict` key is never WTF-8.
                onix_core::Value::Str(s) => {
                    onix_core::value::ObjectKey::Str(onix_core::value::Key::Utf8(
                        builder.intern(
                            s.as_utf8()
                                .expect("a golden fixture's $dict key is always valid UTF-8"),
                        ),
                    ))
                }
                _ => onix_core::value::ObjectKey::Other(Box::new(decoded_key)),
            };

            (key, decode_tagged(item, builder))
        })
        .collect();

    builder.object_with_keys(entries)
}

/// Decodes an `$object` fixture — a custom object (issue #66): its payload is
/// `{"class": "<name>", "attrs": {…}, ["identity": "<module.qualname>"]}`.
/// Builds the same class-tagged, attribute-diffed value onix's own bindings
/// build for a live instance. `identity` (the qualified type identity onix
/// decides `type_changes` by) defaults to `class` when a fixture omits it —
/// every committed object fixture uses a distinct `class` per distinct type, so
/// name and identity coincide for them; the same-name-different-identity cases
/// are pinned by the Python differential suite against live `DeepDiff` instead.
fn decode_tagged_object(
    map: &serde_json::Map<String, Value>,
    builder: &mut onix_core::value::Builder,
) -> onix_core::Value {
    let Some(Value::Object(payload)) = map.get("$object") else {
        panic!("the \"$object\" tag's payload must be an object");
    };
    let Some(Value::String(class)) = payload.get("class") else {
        panic!("an \"$object\" tag must carry a string \"class\"");
    };
    let Some(Value::Object(attrs)) = payload.get("attrs") else {
        panic!("an \"$object\" tag must carry an \"attrs\" object");
    };
    let identity = match payload.get("identity") {
        Some(Value::String(id)) => id.as_str(),
        Some(_) => panic!("an \"$object\" tag's \"identity\" must be a string"),
        None => class.as_str(),
    };

    let entries: Vec<(onix_core::value::ObjectKey, onix_core::Value)> = attrs
        .iter()
        .map(|(name, item)| {
            (
                onix_core::value::ObjectKey::Str(onix_core::value::Key::Utf8(builder.intern(name))),
                decode_tagged(item, builder),
            )
        })
        .collect();

    let dict_len = entries.len();
    builder.custom_object(
        entries,
        std::sync::Arc::from(class.as_str()),
        std::sync::Arc::from(identity),
        onix_core::value::ObjectLengths {
            dict_len,
            ..Default::default()
        },
        Vec::new(),
        None,
    )
}

/// The decoded members of a `$set`/`$frozenset` fixture; panics on two structurally equal members.
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

/// The decoded items of a tagged sequence, panicking if the payload is not an array.
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

/// The string payload of a tagged scalar, panicking if it is not a string.
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

/// Diffs a case's `a.json` against `b.json` and renders the report.
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

/// Cases whose `DeepDiff` survivor onix does not reproduce; each has its own test.
/// See `tests/golden/README.md`.
const KNOWN_DIVERGENT_CASES: &[&str] = &["path_rendering_collision"];

/// Crash-class cases (`deepdiff_raises` markers), each pinned by its own test.
const DEEPDIFF_CRASH_CASES: &[&str] = &["ignore_order_big_int_beyond_f64_deepdiff_overflows"];

/// Whether `expected` is a `deepdiff_raises` crash marker.
fn is_deepdiff_crash_case(expected: &Value) -> bool {
    expected.get("deepdiff_raises").is_some()
}

/// Every golden case not listed in [`KNOWN_DIVERGENT_CASES`] must match its
/// `expected.json` exactly. Failures across the *whole* corpus are
/// collected and reported together.
#[test]
fn every_golden_case_matches_deepdiff() {
    let mut failures = Vec::new();

    for name in case_names() {
        if KNOWN_DIVERGENT_CASES.contains(&name.as_str()) {
            continue;
        }

        let case_dir = golden_root().join(&name);
        let expected_raw = read_json(&case_dir.join("expected.json"));
        if is_deepdiff_crash_case(&expected_raw) {
            continue;
        }
        let expected = collapse_bigint_tags(expected_raw);
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

/// A dict key containing `']['` syntax collapses without panicking and the report
/// stays DeepDiff-shaped.
#[test]
fn path_rendering_collision_does_not_panic_and_is_deepdiff_shaped() {
    let actual = diff_case("path_rendering_collision");

    // onix's sorted traversal keeps the top-level entry.
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

/// Pins onix's result for a case `DeepDiff` crashes on: the pair reports `values_changed`.
/// Values beyond `f64` render as `null` through `to_json_value`.
#[test]
fn ignore_order_big_int_beyond_f64_pairs_without_panicking() {
    let actual = diff_case("ignore_order_big_int_beyond_f64_deepdiff_overflows");
    assert_eq!(
        actual,
        serde_json::json!({
            "values_changed": {"root[0]": {"new_value": null, "old_value": null}}
        })
    );
}

#[test]
fn every_deepdiff_crash_case_is_pinned() {
    for name in case_names() {
        let expected = read_json(&golden_root().join(&name).join("expected.json"));
        if is_deepdiff_crash_case(&expected) {
            assert!(
                DEEPDIFF_CRASH_CASES.contains(&name.as_str()),
                "crash-class case {name:?} has no dedicated pin test; add it to \
                 DEEPDIFF_CRASH_CASES and pin onix's result"
            );
        }
    }
}

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

    // The test-only decoder gives the container instead, likewise for `$set` and `$frozenset`.
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
