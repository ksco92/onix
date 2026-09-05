//! `FxHash`: a small, fast, non-cryptographic hasher used for this module's
//! `HashMap`/`HashSet`s ([`HashedList::info`](super::hash::HashedList),
//! `AddedCandidates::buckets`, the
//! pairing/`used` sets in [`compute_pairs`](super::pairing::compute_pairs),
//! and the [`IgnoreOrderMemo`](super::memo::IgnoreOrderMemo) cache) —
//! chosen for speed at the cost of hash-flooding resistance. Some of these
//! maps key on attacker-controlled data, so this is a deliberate,
//! measured `DoS` trade-off, not a free choice; see [`FxHasher`]'s own doc for
//! the exact threat model, the measured cost of the safe alternative, and
//! why the trade is accepted here.

use std::hash::BuildHasherDefault;

/// A [`std::collections::HashMap`] keyed by this module's own types
/// ([`ItemKey`](super::hash::ItemKey), [`Distance`](super::distance::Distance)), using [`FxHasher`] instead of the standard
/// library's default (`SipHash`) — see that type's doc for the trade-off.
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
/// # `DoS` trade-off (this hasher is *not* collision-resistant)
///
/// `FxHash` uses a fixed, public seed ([`FX_SEED`]) and an invertible step, so
/// an adversary who controls the hashed values can compute keys that all fall
/// in one bucket and degrade any `FxHash` `HashMap`/`HashSet` from `O(1)` to
/// `O(n)` per operation — worst case pushing
/// [`HashedList::build`](super::hash::HashedList::build) from `O(n)` to
/// `O(n²)` on a crafted all-colliding list, on top of the module's already
/// `O(N²)` pairing (see the parent module doc's complexity note).
///
/// **Scope: which tables are collision-immune, and why.** The set-member
/// interning tables in [`IgnoreOrderMemo`](super::memo::IgnoreOrderMemo)
/// (`node_table`, `member_content`) are deliberately **not** `FxHash` — they are
/// [`BTreeMap`](std::collections::BTreeMap)s. They are keyed by
/// attacker-controlled member content *and* reached for every set/frozenset
/// comparison, **including with the default `ignore_order=false`**, so an
/// `FxHash` table there would be a crafted-collision `DoS` on the ordinary
/// path; a `BTreeMap` has no hash to attack (`O(log n)` worst case, always). See
/// that module's "Set-member digests" section. Every remaining `FxHash` table
/// in this module — `HashedList`, `AddedCandidates`,
/// the pairing/`used` sets, and the distance memo — is reached **only** under
/// `ignore_order=true`, the pairing hot path already bounded by the `O(N²)`
/// caveat below; those keep `FxHash`, and their float-carrying keys
/// ([`ItemKey`](super::hash::ItemKey), [`ScalarKey`](crate::lcs::ScalarKey),
/// and the distance memo's [`DistKey`](super::hash::DistKey)) mix their bits
/// first ([`mix_float_bits`](crate::lcs::mix_float_bits)) — `DistKey`'s
/// encoding routes every float through `number_key`, hence `ItemKey::Float`,
/// hence `mix_float_bits`, so the hazard does not reach the new key either — a
/// *non-adversarial* run of integral/half-integer floats, whose raw bit
/// patterns share ~50 trailing zeros, does not accidentally collide.
///
/// For those remaining `ignore_order`-only tables the trade is deliberate and
/// measured. Re-keying them to `SipHash` (`RandomState`) added a material,
/// measured double-digit-percentage per-call cost on the pairing-heavy
/// `ignore_order` benchmark shapes (`change_n` ≈500 on each side → ~250,000
/// candidate pairs, each touching several of these maps) — a permanent cost on
/// the module's common case to defend a worst case that (a) mirrors upstream
/// `DeepDiff`'s own un-bounded `O(N²)` behavior and (b) only matters for callers
/// feeding untrusted input, who must already bound input *size* against that
/// `O(N²)` pairing regardless of hasher. A caller processing untrusted JSON
/// should cap input size before enabling `ignore_order` (the size bound that
/// tames the `O(N²)` pairing also tames this). Upstream `DeepDiff` hashes
/// `str`/`bytes` with a per-process-random `SipHash` seed (`PYTHONHASHSEED`);
/// the default-reachable set-member path now matches that resistance via
/// `BTreeMap`.
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
