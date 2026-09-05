use super::Tag;
use crate::test_support::{cdate, cdt, cdt_at, cv, cvec};
use serde_json::json;

// Thin wrappers routing each `serde_json`-literal-based test through the real
// compact-typed engine via the shared `crate::test_support` converters.
fn all_basic_scalars(items: &[serde_json::Value]) -> bool {
    super::all_basic_scalars(&cvec(items))
}
fn compute_opcodes(a: &[serde_json::Value], b: &[serde_json::Value]) -> Vec<super::Opcode> {
    super::compute_opcodes(&cvec(a), &cvec(b))
}
fn scalar_key(value: &serde_json::Value) -> super::ScalarKey {
    super::scalar_key(&cv(value))
}

/// Python-`==` equality for two JSON scalars, per [`super::ScalarKey`]'s
/// doc. Test-only: the engine compares scalars directly by
/// [`super::ScalarKey`] (including [`super::find_longest_match`]'s autojunk
/// extension step) rather than through a standalone predicate — this survives
/// here purely to assert the hashability/cross-type-equality semantics
/// directly, and to state the `Replace`-opcode non-matching-pair invariant
/// precisely in [`replace_opcode_ranges_never_share_a_matching_element`].
fn python_scalar_eq(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    scalar_key(a) == scalar_key(b)
}

fn grouped_opcodes(
    a: &[serde_json::Value],
    b: &[serde_json::Value],
    n: usize,
) -> Vec<Vec<super::Opcode>> {
    super::grouped_opcodes(&cvec(a), &cvec(b), n)
}

// --- all_basic_scalars ---------------------------------------------

#[test]
fn empty_slice_is_vacuously_all_scalar() {
    assert!(all_basic_scalars(&[]));
}

#[test]
fn every_json_scalar_kind_qualifies() {
    assert!(all_basic_scalars(&[
        serde_json::Value::Null,
        json!(true),
        json!(1),
        json!(1.5),
        json!("s"),
    ]));
}

#[test]
fn a_nested_array_disqualifies() {
    assert!(!all_basic_scalars(&[json!(1), json!([1])]));
}

#[test]
fn a_nested_object_disqualifies() {
    assert!(!all_basic_scalars(&[json!(1), json!({"a": 1})]));
}

// --- python_scalar_eq (the hashability/cross-type finding) ---------

#[test]
fn int_and_equal_float_are_python_equal() {
    assert!(python_scalar_eq(&json!(1), &json!(1.0)));
}

#[test]
fn bool_and_equal_int_are_python_equal() {
    assert!(python_scalar_eq(&json!(true), &json!(1)));
    assert!(python_scalar_eq(&json!(false), &json!(0)));
}

#[test]
fn positive_and_negative_zero_are_python_equal() {
    assert!(python_scalar_eq(&json!(0.0), &json!(-0.0)));
}

#[test]
fn different_numeric_values_are_not_python_equal() {
    assert!(!python_scalar_eq(&json!(1), &json!(2)));
}

#[test]
fn string_and_number_are_never_python_equal() {
    assert!(!python_scalar_eq(&json!("1"), &json!(1)));
}

#[test]
fn null_is_only_python_equal_to_null() {
    assert!(python_scalar_eq(
        &serde_json::Value::Null,
        &serde_json::Value::Null
    ));
    assert!(!python_scalar_eq(&serde_json::Value::Null, &json!(0)));
}

#[test]
fn non_integral_floats_compare_by_value() {
    assert!(python_scalar_eq(&json!(1.5), &json!(1.5)));
    assert!(!python_scalar_eq(&json!(1.5), &json!(2.5)));
}

// --- compute_opcodes -------------------------------------------------

fn vals(items: &[i64]) -> Vec<serde_json::Value> {
    items.iter().map(|&i| json!(i)).collect()
}

fn vals_str(items: &[&str]) -> Vec<serde_json::Value> {
    items.iter().map(|&s| json!(s)).collect()
}

#[test]
fn equal_lists_are_one_equal_opcode() {
    let a = vals(&[1, 2, 3]);
    let ops = compute_opcodes(&a, &a);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].tag, Tag::Equal);
    assert_eq!((ops[0].a1, ops[0].a2, ops[0].b1, ops[0].b2), (0, 3, 0, 3));
}

#[test]
fn empty_a_is_one_insert_opcode() {
    let a: Vec<serde_json::Value> = Vec::new();
    let b = vals(&[1, 2, 3]);
    let ops = compute_opcodes(&a, &b);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].tag, Tag::Insert);
    assert_eq!((ops[0].a1, ops[0].a2, ops[0].b1, ops[0].b2), (0, 0, 0, 3));
}

#[test]
fn empty_b_is_one_delete_opcode() {
    let a = vals(&[1, 2, 3]);
    let b: Vec<serde_json::Value> = Vec::new();
    let ops = compute_opcodes(&a, &b);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].tag, Tag::Delete);
    assert_eq!((ops[0].a1, ops[0].a2, ops[0].b1, ops[0].b2), (0, 3, 0, 0));
}

#[test]
fn both_empty_is_no_opcodes() {
    let a: Vec<serde_json::Value> = Vec::new();
    assert!(compute_opcodes(&a, &a).is_empty());
}

/// Real `DeepDiff` matches this as an insert of `True` at
/// the front plus a delete of the trailing `False` — see
/// `crate::diff`'s module doc and `tests/golden/list_lcs_repro_bool_reorder`.
#[test]
fn repro_bool_reorder_matches_real_deepdiff_opcodes() {
    let a = vec![json!(false), json!(true), json!(false)];
    let b = vec![json!(true), json!(false), json!(true)];
    let ops = compute_opcodes(&a, &b);
    assert_eq!(
        ops,
        vec![
            super::Opcode {
                tag: Tag::Insert,
                a1: 0,
                a2: 0,
                b1: 0,
                b2: 1,
            },
            super::Opcode {
                tag: Tag::Equal,
                a1: 0,
                a2: 2,
                b1: 1,
                b2: 3,
            },
            super::Opcode {
                tag: Tag::Delete,
                a1: 2,
                a2: 3,
                b1: 3,
                b2: 3,
            },
        ]
    );
}

#[test]
fn equal_length_replace_is_one_replace_opcode() {
    let a = vals(&[1, 2, 3]);
    let b = vals(&[1, 5, 3]);
    let ops = compute_opcodes(&a, &b);
    assert_eq!(
        ops,
        vec![
            super::Opcode {
                tag: Tag::Equal,
                a1: 0,
                a2: 1,
                b1: 0,
                b2: 1,
            },
            super::Opcode {
                tag: Tag::Replace,
                a1: 1,
                a2: 2,
                b1: 1,
                b2: 2,
            },
            super::Opcode {
                tag: Tag::Equal,
                a1: 2,
                a2: 3,
                b1: 2,
                b2: 3,
            },
        ]
    );
}

/// A `Replace` opcode's two ranges never share a matching element (see
/// [`compute_opcodes`]'s doc) — verified directly here by brute force
/// over every `Replace` opcode `compute_opcodes` produces for a
/// spread of small random-ish inputs, not just asserted in prose.
#[test]
fn replace_opcode_ranges_never_share_a_matching_element() {
    let cases: &[(&[i64], &[i64])] = &[
        (&[0, 0, 3, 3, 0, 1, 0], &[4, 3, 0, 4]),
        (&[1, 2, 1], &[2, 1, 2]),
        (&[1, 2, 3], &[0, 3, 1, 2]),
        (&[5, 1, 2, 3], &[1, 2, 3, 5]),
    ];
    for (a, b) in cases {
        let a = vals(a);
        let b = vals(b);
        for op in compute_opcodes(&a, &b) {
            if op.tag != Tag::Replace {
                continue;
            }
            for x in &a[op.a1..op.a2] {
                for y in &b[op.b1..op.b2] {
                    assert!(
                        !python_scalar_eq(x, y),
                        "replace opcode {op:?} contains a matching pair {x:?}/{y:?}"
                    );
                }
            }
        }
    }
}

/// `get_matching_blocks`' right-recursion bound
/// (`match_a + match_size < ahi && match_b + match_size < bhi`) needs
/// its `+`, not a `*`: found by mutation testing, minimized from a
/// randomized differential-test failure against a mutated build. With
/// either `+` replaced by `*`,
/// this pair's second `insert` opcode (`a[3:3]` / `b[6:7]`) is missed
/// entirely, changing the a-side split point of the trailing `equal`
/// block and, downstream in `array_diff`, silently dropping the
/// `iterable_item_added` at `root[6]` for a `values_changed` at
/// `root[3]` instead — confirmed against real `deepdiff==9.1.0`.
#[test]
fn get_matching_blocks_right_recursion_finds_a_second_insert() {
    let a = vals(&[1, 0, 0, 0]);
    let b = vals(&[0, 0, 0, 1, 0, 0, 1, 0]);

    assert_eq!(
        compute_opcodes(&a, &b),
        vec![
            super::Opcode {
                tag: Tag::Insert,
                a1: 0,
                a2: 0,
                b1: 0,
                b2: 3,
            },
            super::Opcode {
                tag: Tag::Equal,
                a1: 0,
                a2: 3,
                b1: 3,
                b2: 6,
            },
            super::Opcode {
                tag: Tag::Insert,
                a1: 3,
                a2: 3,
                b1: 6,
                b2: 7,
            },
            super::Opcode {
                tag: Tag::Equal,
                a1: 3,
                a2: 4,
                b1: 7,
                b2: 8,
            },
        ]
    );
}

/// `get_matching_blocks`' LEFT-recursion bound
/// (`alo < match_a && blo < match_b`) is fine as-is — each condition
/// only guards against enqueuing an empty sub-range, and
/// `find_longest_match` on an empty range provably returns a size-0
/// match, which is a downstream no-op regardless of the other
/// operand, so its `&&`/`<` variants are equivalent mutants, not
/// killable by any test — but the analogous
/// right-recursion arithmetic (`match_a + match_size`, not
/// `match_a * match_size`) needs its own dedicated case distinct from
/// [`get_matching_blocks_right_recursion_finds_a_second_insert`]
/// (that one only kills the `match_b` addition, not this one) — found
/// and minimized the same way.
#[test]
fn get_matching_blocks_right_recursion_finds_a_trailing_insert() {
    let a = vec![
        json!(2),
        json!(0.0),
        json!(1.0),
        json!(2.0),
        json!(false),
        json!(1),
    ];
    let b = vec![json!(1.0), json!(2.0), json!(0.0), json!(0.0), json!(true)];

    assert_eq!(
        compute_opcodes(&a, &b),
        vec![
            super::Opcode {
                tag: Tag::Delete,
                a1: 0,
                a2: 2,
                b1: 0,
                b2: 0,
            },
            super::Opcode {
                tag: Tag::Equal,
                a1: 2,
                a2: 5,
                b1: 0,
                b2: 3,
            },
            super::Opcode {
                tag: Tag::Insert,
                a1: 5,
                a2: 5,
                b1: 3,
                b2: 4,
            },
            super::Opcode {
                tag: Tag::Equal,
                a1: 5,
                a2: 6,
                b1: 4,
                b2: 5,
            },
        ]
    );
}

#[test]
fn one_vs_one_point_zero_is_a_single_equal_opcode() {
    // The hashability finding: 1 and 1.0 match as 'equal', never a
    // replace/type-change opcode.
    let a = vec![json!(1)];
    let b = vec![json!(1.0)];
    let ops = compute_opcodes(&a, &b);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].tag, Tag::Equal);
}

// --- get_matching_blocks: adjacent-block collapsing -----------------
//
// `raw_matches` from the work-stack recursion can land two matches
// right up against each other (found via *different* stack entries,
// e.g. one via the top-level scan and another via a right-recursion
// exploring the remainder) that must collapse into one — the three
// cases below were each minimized from a randomized differential-test
// failure against a specific mutated build (the collapse check's
// `&&`, and each side's `pending_a`/`pending_b` `+`, mutated to `||`/
// `*` respectively).

#[test]
fn adjacent_matches_from_different_stack_entries_collapse_needs_and_not_or() {
    // A single insert followed by a single equal: two raw matches
    // ((0,0,0) the initial no-match state contributes nothing, and
    // (0,1,1) the real "s0" match) that must NOT spuriously collapse
    // with an unrelated leading segment. `&&` mutated to `||` moves
    // the added item from `root[0]` to `root[1]` — wrong on either
    // count (a's only element is unambiguously still present).
    let a = vals_str(&["s0"]);
    let b = vals_str(&["s2", "s0"]);

    assert_eq!(
        compute_opcodes(&a, &b),
        vec![
            super::Opcode {
                tag: Tag::Insert,
                a1: 0,
                a2: 0,
                b1: 0,
                b2: 1,
            },
            super::Opcode {
                tag: Tag::Equal,
                a1: 0,
                a2: 1,
                b1: 1,
                b2: 2,
            },
        ]
    );
}

#[test]
fn collapse_check_needs_pending_a_plus_pending_size_not_times() {
    let a = vals(&[2, 2, 0, 1, 1, 0, 1]);
    let b = vals(&[1, 1, 1]);

    assert_eq!(
        compute_opcodes(&a, &b),
        vec![
            super::Opcode {
                tag: Tag::Delete,
                a1: 0,
                a2: 3,
                b1: 0,
                b2: 0,
            },
            super::Opcode {
                tag: Tag::Equal,
                a1: 3,
                a2: 5,
                b1: 0,
                b2: 2,
            },
            super::Opcode {
                tag: Tag::Delete,
                a1: 5,
                a2: 6,
                b1: 2,
                b2: 2,
            },
            super::Opcode {
                tag: Tag::Equal,
                a1: 6,
                a2: 7,
                b1: 2,
                b2: 3,
            },
        ]
    );
}

#[test]
fn collapse_check_needs_pending_b_plus_pending_size_not_times() {
    let a = vec![json!(0), json!(2), json!(2)];
    let b = vec![
        json!(false),
        json!(1.0),
        json!(2),
        json!(0),
        json!(2.0),
        json!(0),
        json!(2),
    ];

    assert_eq!(
        compute_opcodes(&a, &b),
        vec![
            super::Opcode {
                tag: Tag::Insert,
                a1: 0,
                a2: 0,
                b1: 0,
                b2: 3,
            },
            super::Opcode {
                tag: Tag::Equal,
                a1: 0,
                a2: 2,
                b1: 3,
                b2: 5,
            },
            super::Opcode {
                tag: Tag::Insert,
                a1: 2,
                a2: 2,
                b1: 5,
                b2: 6,
            },
            super::Opcode {
                tag: Tag::Equal,
                a1: 2,
                a2: 3,
                b1: 6,
                b2: 7,
            },
        ]
    );
}

// --- datetimes and dates -------------------------------------------------

#[test]
fn calendar_values_are_basic_scalars() {
    // DeepDiff's `helper.basic_types` lists `datetime.datetime` and
    // `datetime.date`, so a list of them takes the difflib path.
    assert!(super::all_basic_scalars(&[
        cdt(2024, 1, 1, None),
        cdate(2024, 1, 1),
        cv(&json!(1)),
    ]));
}

#[test]
fn datetime_scalar_keys_follow_pythons_own_equality_not_the_engines() {
    let naive = super::python_scalar_key(&cdt_at(2024, 1, 1, 10, 0, 0, 0, None));
    let utc = super::python_scalar_key(&cdt_at(2024, 1, 1, 10, 0, 0, 0, Some(0)));
    let plus_two = super::python_scalar_key(&cdt_at(2024, 1, 1, 12, 0, 0, 0, Some(2 * 3600)));

    // Two aware values at one instant are Python-equal...
    assert_eq!(utc, plus_two);
    // ...but a naive value never equals an aware one, however the engine's
    // own instant comparison reads it.
    assert_ne!(naive, utc);
}

#[test]
fn a_date_key_never_equals_a_datetime_key_or_another_dates() {
    let date = super::python_scalar_key(&cdate(2024, 1, 1));

    assert_ne!(date, super::python_scalar_key(&cdt(2024, 1, 1, None)));
    assert_ne!(date, super::python_scalar_key(&cdate(2024, 1, 2)));
    assert_eq!(date, super::python_scalar_key(&cdate(2024, 1, 1)));
}

/// `mix_float_bits` must spread the low bits of integral and half-integer
/// floats — whose raw bit patterns share ~50 trailing zeros — so they do not
/// all fall in one hash bucket (which would make the crate's `FxHash`-backed
/// interning tables degrade to a linear scan; see the function's own doc).
/// Deterministic, so this guards the mixing without a timing measurement.
#[test]
fn mix_float_bits_spreads_low_bits_of_integral_and_half_integer_floats() {
    use std::collections::HashSet;
    for &half in &[0.0_f64, 0.5] {
        let low_bytes: HashSet<u8> = (0..1000u64)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let f = i as f64 + half;
                #[allow(clippy::cast_possible_truncation)]
                {
                    (super::mix_float_bits(f.to_bits()) & 0xff) as u8
                }
            })
            .collect();
        // The raw bit patterns share their low byte (all zeros); mixed, 1000
        // of them must cover most of the 256 possible low bytes. A no-op mix
        // would leave a single value here.
        assert!(
            low_bytes.len() > 200,
            "mixed low byte covered only {} of 256 values for +{half}",
            low_bytes.len()
        );
    }
    // Distinct inputs stay distinct (the mix is injective enough for a key).
    assert_ne!(
        super::mix_float_bits(1.0_f64.to_bits()),
        super::mix_float_bits(2.0_f64.to_bits())
    );
}

// --- grouped_opcodes -----------------------------------------------

#[test]
fn grouped_opcodes_of_empty_inputs_yields_no_groups() {
    // `get_opcodes` is empty for two empty sequences, so the fallback dummy
    // "equal" opcode is inserted and then dropped as a trivial single-equal
    // group — no group is emitted. (`unified_diff` never reaches this, since
    // its trigger requires a newline, but the port mirrors difflib exactly.)
    assert!(grouped_opcodes(&[], &[], 3).is_empty());
}

#[test]
fn grouped_opcodes_of_identical_inputs_yields_no_groups() {
    let same = vec![json!("a"), json!("b"), json!("c")];
    assert!(grouped_opcodes(&same, &same, 3).is_empty());
}

#[test]
fn grouped_opcodes_splits_far_apart_changes_into_separate_groups() {
    // Changes at index 1 and 15 with >2n unchanged lines between them, so the
    // long equal run splits the opcodes into two groups (difflib's cluster
    // isolation).
    let a: Vec<serde_json::Value> = (0..20).map(|i| json!(format!("L{i}"))).collect();
    let b: Vec<serde_json::Value> = (0..20)
        .map(|i| match i {
            1 => json!("X1"),
            15 => json!("X15"),
            _ => json!(format!("L{i}")),
        })
        .collect();
    assert_eq!(grouped_opcodes(&a, &b, 3).len(), 2);
}

// --- find_longest_match extension step (autojunk-only) --------------
//
// These call `find_longest_match` directly with a `b2j` that deliberately
// omits a "popular" element (as `build_b2j`'s autojunk purge would), so the
// DP chain cannot match that element and only the greedy extension step can
// re-bridge it. The window and match offsets are asymmetric between the two
// sides so each loop bound (`best_a > alo`, `best_b > blo`, and the two
// forward `< ahi`/`< bhi` checks) is exercised as the binding constraint.

fn scalar_keys(
    items: &[serde_json::Value],
) -> std::collections::HashMap<super::ScalarKey, Vec<usize>> {
    // A b2j built by hand so a chosen key can be omitted (purged).
    let mut map: std::collections::HashMap<super::ScalarKey, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, v) in items.iter().enumerate() {
        map.entry(scalar_key(v)).or_default().push(i);
    }
    map
}

#[test]
fn extension_bridges_a_purged_element_backward() {
    // a = [P, P, u], b = [P, u]; P is purged from b2j, so the DP only finds
    // `u` (a[2] == b[1]); the backward extension must re-bridge one `P` to
    // give the full match a[1..3] == b[0..2].
    let a = cvec(&[json!("P"), json!("P"), json!("u")]);
    let b = cvec(&[json!("P"), json!("u")]);
    let mut b2j = scalar_keys(&[json!("P"), json!("u")]);
    b2j.remove(&scalar_key(&json!("P")));
    let window = super::Window {
        alo: 0,
        ahi: a.len(),
        blo: 0,
        bhi: b.len(),
    };
    assert_eq!(
        super::find_longest_match(&a, &b, window, &b2j, true),
        (1, 0, 2),
    );
}

#[test]
fn extension_bridges_a_purged_element_forward() {
    // a = [u, P], b = [u, P, P]; P purged, DP finds only `u` (a[0] == b[0]);
    // the forward extension must re-bridge one `P`, giving a[0..2] == b[0..2].
    let a = cvec(&[json!("u"), json!("P")]);
    let b = cvec(&[json!("u"), json!("P"), json!("P")]);
    let mut b2j = scalar_keys(&[json!("u"), json!("P"), json!("P")]);
    b2j.remove(&scalar_key(&json!("P")));
    let window = super::Window {
        alo: 0,
        ahi: a.len(),
        blo: 0,
        bhi: b.len(),
    };
    assert_eq!(
        super::find_longest_match(&a, &b, window, &b2j, false),
        (0, 0, 1),
        "with extend=false the purged P is not bridged"
    );
    assert_eq!(
        super::find_longest_match(&a, &b, window, &b2j, true),
        (0, 0, 2),
        "with extend=true the forward extension bridges one P"
    );
}

// --- build_b2j autojunk purge --------------------------------------

#[test]
fn build_b2j_purges_only_above_the_autojunk_threshold() {
    // 200 elements: `ntest = 200 / 100 + 1 = 3`. "pop" (4 occurrences) is
    // purged; "keep3" (exactly 3) is kept, as is every unique filler.
    let mut items: Vec<serde_json::Value> = Vec::new();
    items.extend(std::iter::repeat_n(json!("pop"), 4));
    items.extend(std::iter::repeat_n(json!("keep3"), 3));
    for i in 0..193 {
        items.push(json!(format!("u{i}")));
    }
    assert_eq!(items.len(), 200);
    let b = cvec(&items);

    let purged = super::build_b2j(&b, true);
    assert!(
        !purged.contains_key(&scalar_key(&json!("pop"))),
        "pop occurs 4 times (> ntest 3) and must be purged"
    );
    assert!(
        purged.contains_key(&scalar_key(&json!("keep3"))),
        "keep3 occurs exactly 3 times (== ntest) and must be kept"
    );

    // With autojunk off, nothing is ever purged even past 200 elements.
    let unpurged = super::build_b2j(&b, false);
    assert!(unpurged.contains_key(&scalar_key(&json!("pop"))));
}
