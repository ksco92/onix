//! `FxHash`: a small, fast, non-cryptographic hasher for this module's `HashMap`/`HashSet`s.
//! See [`FxHasher`]'s doc for the accepted `DoS` trade-off and which tables carry it.

use std::hash::BuildHasherDefault;

/// This module's [`HashMap`](std::collections::HashMap), keyed with [`FxHasher`].
pub(crate) type HashMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FxHasher>>;
/// The [`HashMap`] equivalent for [`std::collections::HashSet`].
pub(crate) type HashSet<T> = std::collections::HashSet<T, BuildHasherDefault<FxHasher>>;

/// The `FxHash` algorithm (the one `rustc` itself uses for compiler-hot-path hash maps),
/// implemented from scratch: no new-dependency budget for a handful of lines.
///
/// # `DoS` trade-off (this hasher is *not* collision-resistant)
///
/// `FxHash` uses a fixed, public seed ([`FX_SEED`]) and an invertible step, letting an
/// adversary force every key into one bucket ([`HashedList`](super::hash::HashedList),
/// `AddedCandidates`, the pairing/`used` sets, and the distance memo in
/// [`IgnoreOrderMemo`](super::memo::IgnoreOrderMemo) are `FxHash`-keyed, reached only under
/// `ignore_order=true`): an accepted, documented `DoS` trade-off, not an oversight —
/// `SipHash` there cost a real, measured per-call penalty on the pairing hot path (PR #4).
/// Bound untrusted input against the module's `O(N²)` pairing regardless of hasher.
///
/// `node_table` and `member_content` in `IgnoreOrderMemo` are `BTreeMap`s instead — reached
/// on the default path too, no `Hash` derive, `O(log n)` worst case — though each comparison
/// walks the whole key (`member_content`'s `MemberContent::UnhashableDict` key nests each
/// dict key's own `ItemKey` tree). An integer beyond `i128` (`ItemKey::BigInt`)
/// hashes/compares by magnitude digits, `O(digits)` not `O(1)`, uncapped by the
/// `ignore_order` element-count limit: one huge integer costs its own digit length regardless
/// of element count. A custom object's `ItemKey::Object` costs `ItemKey::Dict`'s full walk
/// plus one class-name comparison; `ItemKey::Opaque` costs one identity string comparison.
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
