//! The get-pairs gate threshold and the greedy candidate-pairing algorithm
//! ([`compute_pairs`]) it feeds — `DeepDiff`'s
//! `_get_most_in_common_pairs_in_iterables`. Ranks candidate
//! `(added, removed)` pairs by `super::distance::rough_distance` and
//! resolves the many-to-many candidate graph into a one-to-one matching
//! with the exact greedy, asymmetrically-tie-broken rule described on
//! [`compute_pairs`]'s own doc.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::diff::DiffOptions;

use super::IgnoreOrderMemo;
use super::distance::{Distance, rough_distance};
use super::fxhash::{HashMap, HashSet};
use super::hash::{DistKey, HashedList, ItemKey};
use super::memo::is_container;

/// `cutoff_distance_for_pairs`'s default (`DeepDiff`'s own name;
/// `CUTOFF_DISTANCE_FOR_PAIRS_DEFAULT`, diff.py) — a candidate pair is
/// rejected outright when its [`rough_distance`] is `>=` this. Same MVP
/// scope note as above.
pub(crate) const CUTOFF_DISTANCE_FOR_PAIRS: f64 = 0.3;

// ---------------------------------------------------------------------
// Item hashing: the canonical equivalence key
// ---------------------------------------------------------------------

/// The removed-hash candidates found for one added hash, grouped by exact
/// [`Distance`] — mirrors `most_in_common_pairs[added_hash]` (a Python
/// `defaultdict(SetOrdered)` keyed by distance; see [`compute_pairs`]'s
/// doc). Whether a given `dist` bucket is new (needed to decide whether to
/// also record this added hash in the caller's global
/// `distances_to_from_hashes[dist]`) is checked by the caller itself, via
/// [`Self::buckets`]' own `contains_key`, immediately before calling
/// [`Self::push`] — so this type tracks nothing beyond the buckets
/// themselves.
#[derive(Default)]
struct AddedCandidates {
    buckets: HashMap<Distance, Vec<Rc<ItemKey>>>,
}

impl AddedCandidates {
    /// Appends `removed_hash` to the bucket for `dist`, creating it if this
    /// is the first candidate at this exact distance for this added hash.
    fn push(&mut self, dist: Distance, removed_hash: Rc<ItemKey>) {
        self.buckets.entry(dist).or_default().push(removed_hash);
    }
}

/// `DeepDiff`'s `_get_most_in_common_pairs_in_iterables` (diff.py) — the
/// greedy, non-globally-optimal nearest-neighbor pairing.
///
/// For every `(added, removed)` combination (`hashes_added` outer,
/// `hashes_removed` inner — both in their own first-occurrence order),
/// candidates within [`CUTOFF_DISTANCE_FOR_PAIRS`] are grouped into
/// `most_in_common_pairs[added][distance]` (an [`AddedCandidates`] per
/// added hash) and, on each *new* bucket, the added hash is also appended to
/// `distances_to_from_hashes[distance]` — together, one pass reproduces the
/// exact insertion order `DeepDiff`'s own two-pass construction produces
/// (build `most_in_common_pairs` fully, *then* flatten it into
/// `distances_to_from_hashes` by iterating it in insertion order): the
/// values and order are identical either way, since the flatten step never
/// depends on anything the first pass hasn't already fixed.
///
/// The outer loop then walks distances **ascending** (free from a
/// [`BTreeMap`]'s own iteration order); each per-distance bucket, and each
/// per-added-hash candidate bucket, is drained **LIFO** (`Vec::pop`, i.e.
/// latest-pushed first) exactly like `SetOrdered.pop()` — this is what
/// produces `DeepDiff`'s documented, load-bearing, *asymmetric* tie-break:
/// among several removed-hash candidates tied at the same distance for one
/// added hash, the **no-`break` overwrite** (every unused candidate popped
/// after the first successful one keeps re-overwriting `pairs[added]`, so
/// the last one processed — LIFO means the *smallest* index — wins) makes
/// the **earliest t1 index** win; among several added hashes competing for
/// the same removed candidate, LIFO draining of the *outer* bucket instead
/// makes the **latest t2 index** get first pick. Replicated here
/// wart-for-wart, including the missing `break`, because real `DeepDiff`'s
/// own byte-exact output depends on it (confirmed empirically against a
/// genuine, non-coincidental tie).
///
/// Returns added-hash → removed-hash (only that one direction: `DeepDiff`'s
/// own `pairs` dict is built symmetrically so `get_other_pair` can look it
/// up from either side, but this port only ever looks up from the added
/// side — see [`ignore_order_array_diff`] — so the reverse direction is
/// never constructed).
pub(crate) fn compute_pairs(
    hashes_added: &[Rc<ItemKey>],
    hashes_removed: &[Rc<ItemKey>],
    t1: &HashedList<'_>,
    t2: &HashedList<'_>,
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> HashMap<Rc<ItemKey>, Rc<ItemKey>> {
    let mut most_in_common_pairs: HashMap<Rc<ItemKey>, AddedCandidates> = HashMap::default();
    let mut distances_to_from_hashes: BTreeMap<Distance, Vec<Rc<ItemKey>>> = BTreeMap::new();

    // The distance cache is keyed by each side's exact structural identity
    // (`DistKey`), not its order/repetition-insensitive `ItemKey` (see
    // `DistKey`'s doc and issue #31). Intern one `DistKey` per *distinct*
    // container candidate here — once per added/removed entry, not once per
    // `A * R` pair — so recording a pair is a refcount bump. Only container
    // candidates get one: a scalar pair's distance never recurses, so it is
    // never memoized.
    let added_dist: Vec<Option<DistKey>> = hashes_added
        .iter()
        .map(|key| is_container(key).then(|| DistKey::new(t2.get(key).1)))
        .collect();
    let removed_dist: Vec<Option<DistKey>> = hashes_removed
        .iter()
        .map(|key| is_container(key).then(|| DistKey::new(t1.get(key).1)))
        .collect();

    for (added_idx, added_key) in hashes_added.iter().enumerate() {
        let (_, added_value) = t2.get(added_key);
        for (removed_idx, removed_key) in hashes_removed.iter().enumerate() {
            let (_, removed_value) = t1.get(removed_key);
            // Memoize container-vs-container candidates — the pairs whose
            // distance is a recursive trial diff and so the ones that
            // re-compute exponentially without a cache. `rough_distance` is a
            // pure function of the two subtrees' content on this path, so a
            // value cached under their exact `DistKey` pair is identical to a
            // fresh one — see the `super::memo` module doc for the proof.
            let distance = match (&removed_dist[removed_idx], &added_dist[added_idx]) {
                (Some(removed_dist_key), Some(added_dist_key)) if memo.caching_enabled() => {
                    let key = (removed_dist_key.clone(), added_dist_key.clone());
                    if let Some(cached) = memo.get(&key) {
                        cached
                    } else {
                        let computed = rough_distance(
                            removed_value,
                            added_value,
                            CUTOFF_DISTANCE_FOR_PAIRS,
                            depth,
                            opts,
                            memo,
                        );
                        memo.put(key, computed);
                        computed
                    }
                }
                _ => rough_distance(
                    removed_value,
                    added_value,
                    CUTOFF_DISTANCE_FOR_PAIRS,
                    depth,
                    opts,
                    memo,
                ),
            };
            if distance >= CUTOFF_DISTANCE_FOR_PAIRS {
                continue;
            }

            let dist = Distance(distance);
            let candidates = most_in_common_pairs
                .entry(Rc::clone(added_key))
                .or_default();
            let is_new_bucket = !candidates.buckets.contains_key(&dist);
            candidates.push(dist, Rc::clone(removed_key));
            if is_new_bucket {
                distances_to_from_hashes
                    .entry(dist)
                    .or_default()
                    .push(Rc::clone(added_key));
            }
        }
    }

    let mut used: HashSet<Rc<ItemKey>> = HashSet::default();
    let mut pairs: HashMap<Rc<ItemKey>, Rc<ItemKey>> = HashMap::default();

    for (&dist, from_hashes) in &mut distances_to_from_hashes {
        while let Some(from_hash) = from_hashes.pop() {
            if used.contains(&from_hash) {
                continue;
            }
            // `from_hash` was pushed into `distances_to_from_hashes[dist]`
            // (this very bucket, this very `dist`) only ever at the exact
            // moment its own `most_in_common_pairs[from_hash][dist]` bucket
            // was first created (see the construction loop above) — nothing
            // ever removes a bucket afterward, only drains it — so this
            // lookup always succeeds. Unlike the depth-guard invariants
            // elsewhere in this crate, this is a closed, input-independent
            // bookkeeping fact about this function's own construction, not
            // something adversarial input could violate — an `.expect()` is
            // the right call here, not a silent fallback.
            let to_hashes = most_in_common_pairs
                .get_mut(&from_hash)
                .and_then(|candidates| candidates.buckets.get_mut(&dist))
                .expect(
                    "from_hash was inserted into this exact distance bucket during construction",
                );
            while let Some(to_hash) = to_hashes.pop() {
                if !used.contains(&to_hash) {
                    used.insert(from_hash.clone());
                    used.insert(to_hash.clone());
                    // No `break`: every further unused candidate popped
                    // here keeps overwriting this entry — see this
                    // function's own doc for why that is load-bearing.
                    pairs.insert(from_hash.clone(), to_hash);
                }
            }
        }
    }

    pairs
}

// ---------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------
