//! `FxHash`: a small, fast, non-cryptographic hasher used for this module's
//! internal `HashMap`/`HashSet`s ([`HashedList::info`](super::hash::HashedList),
//! [`AddedCandidates::buckets`](super::pairing::AddedCandidates), the
//! pairing/`used` sets in [`compute_pairs`](super::pairing::compute_pairs)) —
//! see [`FxHasher`]'s own doc for why the standard library's DoS-resistant
//! default (`SipHash`) is the wrong trade-off here.

use std::hash::BuildHasherDefault;

/// A [`std::collections::HashMap`] keyed by this module's own types
/// ([`ItemKey`], [`Distance`]), using [`FxHasher`] instead of the standard
/// library's default (`SipHash`) — see that type's doc for why.
pub(crate) type HashMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FxHasher>>;
/// The [`HashMap`] equivalent for [`std::collections::HashSet`].
pub(crate) type HashSet<T> = std::collections::HashSet<T, BuildHasherDefault<FxHasher>>;

/// A small, non-cryptographic hasher (the `FxHash` algorithm — the same one
/// `rustc` itself uses internally for compiler-hot-path hash maps, chosen
/// for its "add-rotate-multiply" simplicity, not merely because it is
/// endorsed elsewhere) implemented from scratch rather than pulled in as a
/// dependency: this crate's own quality bar has no new-dependency budget
/// for this port, and the algorithm is a handful of lines.
///
/// The pairing gate's candidate loop performs up to
/// `hashes_added.len() * hashes_removed.len()` hash-map lookups/inserts —
/// for the `ignore_order_10k`-shaped benchmark's worst case (`change_n`
/// ≈500 on each side) that is ~250,000 candidate pairs, each touching
/// several of this module's `HashMap`/`HashSet`s ([`HashedList::info`],
/// [`AddedCandidates::buckets`], the pairing/`used` sets in
/// [`compute_pairs`]). The standard library's default hasher (`SipHash`) is
/// deliberately DoS-resistant, which also makes it markedly slower per call
/// than a simple non-cryptographic hash — a real, measured cost at this
/// candidate count (`SipHash`'s overhead is protection against an adversary
/// choosing hash-map keys to force collisions; this module's `ItemKey`
/// values are derived from *diffed data*, not attacker-chosen hash-map
/// keys in the `DoS` sense, so that protection buys nothing here). No
/// cryptographic or DoS-resistance property is needed for these maps
/// specifically — they never key on unbounded attacker-chosen *strings*
/// used as a raw index (the actual scenario `SipHash` defends), only on
/// bounded, already-depth-checked structural keys.
#[derive(Default)]
pub(crate) struct FxHasher {
    pub(crate) hash: u64,
}

/// `FxHash`'s seed constant (the golden-ratio-derived odd constant the
/// reference implementation uses).
pub(crate) const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    /// Folds one 64-bit word into the running hash: rotate, xor, multiply —
    /// `FxHash`'s entire mixing step.
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
