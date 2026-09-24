//! `FxHash`: a small, fast, non-cryptographic hasher for this module's `HashMap`/`HashSet`s.
//! See [`FxHasher`]'s doc for the accepted `DoS` trade-off and which tables carry it.

use std::hash::BuildHasherDefault;

/// This module's [`HashMap`](std::collections::HashMap), keyed with [`FxHasher`].
pub(crate) type HashMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FxHasher>>;
/// The [`HashMap`] equivalent for [`std::collections::HashSet`].
pub(crate) type HashSet<T> = std::collections::HashSet<T, BuildHasherDefault<FxHasher>>;

/// The `FxHash` algorithm (the one `rustc` itself uses for compiler-hot-path hash maps),
/// implemented from scratch.
///
/// # `DoS` trade-off (this hasher is *not* collision-resistant)
///
/// `FxHash` uses a fixed, public seed ([`FX_SEED`]) and an invertible step: a crafted
/// collision degrades an `FxHash` map or set from `O(1)` to `O(n)` per operation, pushing
/// [`HashedList::build`](super::hash::HashedList::build) from `O(n)` to `O(n²)` on an
/// all-colliding list, on top of the module's `O(N²)` pairing.
/// [`HashedList`](super::hash::HashedList), `AddedCandidates`, the pairing/`used` sets and
/// its result maps (`most_in_common_pairs`, `pairs`), `consumed_removed`, and the distance
/// memo are `FxHash`-keyed and reached only under `ignore_order=true`: an accepted,
/// documented `DoS` trade-off — `SipHash` there cost a measured per-call penalty on the
/// pairing hot path (PR #4). [`IgnoreOrderMemo`](super::memo::IgnoreOrderMemo)'s `tuple_ids`
/// is `FxHash`-keyed too but not `ignore_order`-only: on the Python-object path (JSON has no
/// sets or tuples), a set member that is a custom object holding a tuple-keyed dict entry
/// reaches it on the default path, through `set_member_digest` -> `tuple_keyed` ->
/// `tuple_digest` — a pre-existing gap tracked in issue #136, the same as the key-union set
/// below. The key-union `HashSet<&[u8]>` in
/// [`is_below_threshold_to_diff_deeper`](super::distance::is_below_threshold_to_diff_deeper)
/// is likewise default-path reachable, called by `object_diff` for every unequal dict pair,
/// and tracked in the same issue. Bound untrusted input against the module's `O(N²)` pairing
/// regardless of hasher.
///
/// Every `FxHash`-keyed type carrying a float ([`ItemKey`](super::hash::ItemKey),
/// [`ScalarKey`](crate::lcs::ScalarKey), and the distance memo's
/// [`DistKey`](super::hash::DistKey) via `number_key`) mixes its bits first
/// ([`mix_float_bits`](crate::lcs::mix_float_bits)), so integral and half-integer floats —
/// whose raw bit patterns share many trailing zeros — do not collide by accident. Every `NaN`
/// additionally folds onto one fixed key, matching `DeepHash`'s `str()`-based digest: this
/// changes which values these tables treat as the same item, not their per-lookup cost.
///
/// `node_table` and `member_content` in `IgnoreOrderMemo` are `BTreeMap`s instead, since they
/// are keyed by attacker-controlled member content and reached on the default path too, with
/// no `Hash` derive: `O(log n)` worst case, always, though each comparison still walks the
/// whole probed key — `member_content`'s `MemberContent::UnhashableDict` key is itself keyed
/// by each dict key's own `ItemKey` tree, not a flat string.
///
/// An integer beyond `i128` (`ItemKey::BigInt`) hashes and compares by its magnitude digits,
/// `O(digits)` not `O(1)` per lookup — inside a hashable tuple it reaches the same cost via
/// `ScalarKey::Big` and `tuple_ids`, through `PyHashPart::Scalar` — a cost the `ignore_order`
/// element-count cap does not bound: one huge integer costs its own digit length regardless
/// of element count. A custom object's `ItemKey::Object` costs `ItemKey::Dict`'s full walk
/// plus one class-name comparison, and two distinct classes sharing a `__name__` share its
/// bucket; `ItemKey::Opaque` costs one identity string comparison.
#[derive(Default)]
pub(crate) struct FxHasher {
    pub(crate) hash: u64,
}

/// `FxHash`'s seed constant (golden-ratio-derived odd constant).
pub(crate) const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    /// Folds one word into the hash: rotate, xor, multiply — `FxHash`'s mixing step.
    pub(crate) fn add_to_hash(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(FX_SEED);
    }
}

impl std::hash::Hasher for FxHasher {
    fn write(&mut self, mut bytes: &[u8]) {
        while let Some(chunk) = bytes.get(..8) {
            let word = u64::from_ne_bytes(chunk.try_into().expect("chunk is exactly 8 bytes"));
            self.add_to_hash(word);
            bytes = &bytes[8..];
        }
        for &byte in bytes {
            self.add_to_hash(u64::from(byte));
        }
    }

    fn write_u8(&mut self, i: u8) {
        self.add_to_hash(u64::from(i));
    }

    fn write_u16(&mut self, i: u16) {
        self.add_to_hash(u64::from(i));
    }

    fn write_u32(&mut self, i: u32) {
        self.add_to_hash(u64::from(i));
    }

    fn write_u64(&mut self, i: u64) {
        self.add_to_hash(i);
    }

    fn write_u128(&mut self, i: u128) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "hash mixing only, truncation does not affect correctness, only distribution"
        )]
        {
            self.add_to_hash(i as u64);
            self.add_to_hash((i >> 64) as u64);
        }
    }

    fn write_usize(&mut self, i: usize) {
        self.add_to_hash(i as u64);
    }

    fn finish(&self) -> u64 {
        self.hash
    }
}
