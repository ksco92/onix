use super::{Tag, all_basic_scalars, compute_opcodes, scalar_key};
use serde_json::json;

/// Python-`==` equality for two JSON scalars, per [`super::ScalarKey`]'s
/// doc. Test-only: production code has no remaining use for this as a
/// standalone function (see [`super::find_longest_match`]'s doc on why
/// its own extend-by-direct-comparison step was removed) — it survives
/// here purely to assert the hashability/cross-type-equality semantics
/// directly, and to state the `Replace`-opcode non-matching-pair
/// invariant precisely in [`replace_opcode_ranges_never_share_a_matching_element`].
fn python_scalar_eq(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    scalar_key(a) == scalar_key(b)
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

/// The M6 repro: real `DeepDiff` matches this as an insert of `True` at
/// the front plus a delete of the trailing `False` — see
/// `crate::diff`'s module doc and `tests/golden/list_lcs_repro_bool_reorder`.
#[test]
fn m6_repro_bool_reorder_matches_real_deepdiff_opcodes() {
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
