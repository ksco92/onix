//! The get-pairs distance cutoff and the greedy candidate pairing ([`compute_pairs`]) it feeds:
//! `DeepDiff`'s `_get_most_in_common_pairs_in_iterables`.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::diff::DiffOptions;
use crate::error::Error;
use crate::path::PathSegment;

use super::IgnoreOrderMemo;
use super::distance::{Distance, rough_distance};
use super::fxhash::{HashMap, HashSet};
use super::hash::{DistKey, HashedList, ItemKey};
use super::memo::is_container;

/// `DeepDiff`'s `cutoff_distance_for_pairs` default: a candidate pair whose [`rough_distance`] is
/// `>=` this is rejected.
pub(crate) const CUTOFF_DISTANCE_FOR_PAIRS: f64 = 0.3;

/// The removed-hash candidates for one added hash, grouped by exact [`Distance`]. See
/// `docs/design/ignore-order.md`, "Pair".
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

/// `DeepDiff`'s `_get_most_in_common_pairs_in_iterables`: greedy pairing of `(added, removed)`
/// candidates within [`CUTOFF_DISTANCE_FOR_PAIRS`], draining distance buckets ascending and each
/// bucket LIFO with no `break`. See `docs/design/ignore-order.md`, "Pair".
pub(crate) fn compute_pairs(
    hashes_added: &[Rc<ItemKey>],
    hashes_removed: &[Rc<ItemKey>],
    t1: &HashedList<'_>,
    t2: &HashedList<'_>,
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<HashMap<Rc<ItemKey>, Rc<ItemKey>>, Box<Error>> {
    let mut most_in_common_pairs: HashMap<Rc<ItemKey>, AddedCandidates> = HashMap::default();
    let mut distances_to_from_hashes: BTreeMap<Distance, Vec<Rc<ItemKey>>> = BTreeMap::new();

    // Container candidates get one interned `DistKey` each, so recording a pair is a refcount
    // bump (issue #31).
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
            let (old_idx, removed_value) = t1.get(removed_key);
            // Only container pairs are memoized; the key is content only.
            let cache_key = match (&removed_dist[removed_idx], &added_dist[added_idx]) {
                (Some(removed_dist_key), Some(added_dist_key)) if memo.caching_enabled() => {
                    Some((removed_dist_key.clone(), added_dist_key.clone()))
                }
                _ => None,
            };
            let distance = if let Some(cached) = cache_key.as_ref().and_then(|key| memo.get(key)) {
                cached
            } else {
                let computed = rough_distance(
                    removed_value,
                    added_value,
                    CUTOFF_DISTANCE_FOR_PAIRS,
                    depth,
                    opts,
                    memo,
                )
                .map_err(|error| error.under(&[PathSegment::Index(old_idx)]))?;
                if let Some(key) = cache_key {
                    memo.put(key, computed);
                }
                computed
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
            let to_hashes = most_in_common_pairs
                .get_mut(&from_hash)
                .and_then(|candidates| candidates.buckets.get_mut(&dist))
                .expect(
                    "from_hash was inserted into this exact distance bucket during construction",
                );
            while let Some(to_hash) = to_hashes.pop() {
                if !used.contains(&to_hash) {
                    used.insert(Rc::clone(&from_hash));
                    used.insert(Rc::clone(&to_hash));
                    pairs.insert(Rc::clone(&from_hash), to_hash);
                }
            }
        }
    }

    Ok(pairs)
}
