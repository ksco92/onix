//! Item hashing: the canonical equivalence key ([`ItemKey`]) and the
//! per-list hash table ([`HashedList`]) it feeds, matching `DeepHash`'s
//! default semantics for **item matching** under `ignore_order=True` — see
//! each type's own doc for the exact rules, and the parent module's doc for
//! how this fits into the algorithm end to end.

use std::collections::{BTreeMap, BTreeSet};

use crate::value::Value;

use super::fxhash::HashMap;

/// A canonical hash-equivalence key for one JSON value, matching
/// `DeepHash`'s default semantics for **item matching** under
/// `ignore_order=True` — deliberately **not** the same
/// equivalence [`crate::lcs::all_basic_scalars`]'s scalar-only ordered-list
/// matcher uses:
///
/// - **Numbers are type-tagged**: an `Int`, a `Float`, and a `Bool` never
///   share a key even at the same numeric value (`1`, `1.0`, and `true` are
///   three distinct keys) — the *opposite* of `crate::lcs::ScalarKey`'s
///   Python-`==` collapsing rule, and confirmed against real `DeepDiff`:
///   `[1]` vs `[1.0]` under `ignore_order=True` is `type_changes` (a real
///   pairing recurses and finds a type mismatch), unlike the *ordered* LCS
///   path's `[1]` vs `[1.0]` (which reports nothing at all — the two rules
///   are independent and both faithfully reproduced, in their own modules).
/// - **A nested list's key is order- and count-insensitive**: `[[1,2,3]]`
///   and `[[3,2,1]]` hash identically as list ELEMENTS (their `List` key is
///   a deduplicated `BTreeSet` of child keys), because `DeepHash`'s own
///   `ignore_iterable_order`/`ignore_repetition` default to `True`
///   regardless of the outer `DeepDiff`'s own `ignore_order` flag —
///   this can make two items with genuinely different *contents*
///   (different order, or different duplicate counts) compare as fully
///   "matched" (no report at all) once nested one level inside an
///   `ignore_order` list; that is real, confirmed `DeepDiff` behavior, not
///   a bug in this port.
/// - **A nested dict's key sorts by key**, recursively keying each value the
///   same way — dict *comparison* itself is never affected by
///   `ignore_order`, but a dict nested as a list *element* still
///   needs a canonical, insertion-order-independent key to be hashed at all.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ItemKey {
    Null,
    /// `true`/`false` — tagged distinctly from any integer of the same
    /// value (never collides with `Int`).
    Bool(bool),
    /// A JSON value `serde_json` parsed as an integer (no decimal point or
    /// exponent) — see [`crate::diff`]'s `python_type_name` for the same
    /// int/float split used throughout this crate. Always exact: a
    /// `serde_json::Number`'s non-float representation is always an `i64`
    /// or `u64`, both of which fit losslessly in `i128`.
    Int(i128),
    /// A JSON value `serde_json` parsed as a float, keyed by its exact bit
    /// pattern — kept as its own bucket even when whole-numbered (`5.0`
    /// never collides with `Int(5)`; see this type's own doc).
    Float(u64),
    Str(String),
    /// Order- and count-insensitive: see this type's own doc.
    List(BTreeSet<ItemKey>),
    /// Key-sorted, recursively keyed values.
    Dict(BTreeMap<String, ItemKey>),
}

/// Computes `value`'s [`ItemKey`]. Recurses natively — safe only because
/// every caller in this module first proves `value`'s nesting is within the
/// crate's shared depth budget via [`check_value_depth`] (see this module's
/// "Depth safety" doc section).
pub(crate) fn item_key(value: &Value) -> ItemKey {
    match value {
        Value::Null => ItemKey::Null,
        Value::Bool(b) => ItemKey::Bool(*b),
        Value::Str(s) => ItemKey::Str(s.to_string()),
        Value::Number(n) => {
            if n.is_f64() {
                let bits = n
                    .as_f64()
                    .expect("Number::is_f64 guarantees as_f64 succeeds")
                    .to_bits();
                ItemKey::Float(bits)
            } else if let Some(i) = n.as_i64() {
                ItemKey::Int(i128::from(i))
            } else {
                let u = n
                    .as_u64()
                    .expect("a non-f64 serde_json::Number always has an i64 or u64 repr");
                ItemKey::Int(i128::from(u))
            }
        }
        Value::Array(items) => ItemKey::List(items.iter().map(item_key).collect()),
        Value::Object(map) => ItemKey::Dict(
            map.iter()
                .map(|(k, v)| (k.to_string(), item_key(v)))
                .collect(),
        ),
    }
}

// ---------------------------------------------------------------------
// Per-list hash tables
// ---------------------------------------------------------------------

/// One list's items, hashed via [`item_key`] and reduced to first-occurrence
/// distinct entries — mirrors `DeepDiff`'s own `full_t1_hashtable`/
/// `full_t2_hashtable` (`_create_hashtable`, `{hash: (item, [indexes])}`).
/// Only the *first* index per distinct hash is kept: every
/// `report_repetition=False` code path in `deepdiff/diff.py` reads
/// `.indexes[0]` exclusively (confirmed by direct source reading), so a
/// hash's other occurrences are provably never used for anything in this
/// module's scope.
pub(crate) struct HashedList<'a> {
    /// Distinct keys, in first-occurrence (ascending original index) order
    /// — this is `SetOrdered(full_t{1,2}_hashtable.keys())`'s own iteration
    /// order (a Python dict's insertion order).
    pub(crate) distinct_order: Vec<ItemKey>,
    info: HashMap<ItemKey, (usize, &'a Value)>,
}

impl<'a> HashedList<'a> {
    pub(crate) fn build(items: &'a [Value]) -> Self {
        let mut distinct_order = Vec::new();
        let mut info: HashMap<ItemKey, (usize, &'a Value)> = HashMap::default();

        for (idx, item) in items.iter().enumerate() {
            let key = item_key(item);

            if let std::collections::hash_map::Entry::Vacant(entry) = info.entry(key.clone()) {
                distinct_order.push(key);
                entry.insert((idx, item));
            }
        }

        Self {
            distinct_order,
            info,
        }
    }

    pub(crate) fn contains(&self, key: &ItemKey) -> bool {
        self.info.contains_key(key)
    }

    /// The first-occurrence `(index, value)` for `key`.
    ///
    /// # Panics
    ///
    /// Panics if `key` was never produced by [`Self::build`] on this same
    /// list. Every caller in this module only ever looks up a key drawn
    /// from `self.distinct_order` itself (or a `hashes_added`/
    /// `hashes_removed` slice filtered from it), so this can never actually
    /// fire.
    pub(crate) fn get(&self, key: &ItemKey) -> (usize, &'a Value) {
        self.info[key]
    }
}
