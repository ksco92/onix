// Portions of this module reimplement algorithms from CPython 3.14.6's
// `difflib` standard-library module (`SequenceMatcher` and its autojunk
// heuristic), used under the PSF License Agreement version 2. See
// THIRD-PARTY-NOTICES.md at the repository root.
//! A faithful, from-scratch port of Python's `difflib.SequenceMatcher`
//! opcode algorithm, in the two configurations this engine needs: always
//! `isjunk=None`, and `autojunk` either off ([`compute_opcodes`], for
//! `DeepDiff`'s ordered-list comparison) or on ([`grouped_opcodes`], for the
//! multi-line string diff `unified_diff` produces — see
//! [`mod@crate::unified_diff`]).
//!
//! This module is deliberately *pure*: it knows nothing about
//! [`crate::report::Report`], [`crate::path::PathSegment`], or recursion
//! depth — it only turns two slices of [`crate::value::Value`] scalars into an
//! ordered list of [`Opcode`]s. [`crate::diff::array_diff`] is what maps
//! those opcodes into report findings; see that module's doc for the full,
//! empirically-verified `DeepDiff` list-compat spec this exists to serve.
//!
//! # Why this exists
//!
//! `DeepDiff`'s default (non-`ignore_order`) list comparison is *not*
//! always a simple index-aligned scan: when
//! every element of *both* lists is a scalar (`DeepDiff`'s own
//! `_all_values_basic_hashable` check — null/bool/number/string, plus
//! datetime, date, time and timedelta from `helper.basic_types`'s
//! `datetimes` tuple; a dict or list anywhere in either list disqualifies
//! the whole comparison), it
//! additionally runs a `difflib.SequenceMatcher`-based "cheapest edit"
//! match and, only when that produces more than one finding, compares its
//! finding *count* against the plain index-aligned algorithm's, keeping
//! whichever is smaller (a tie keeps the index-aligned result). See
//! `crate::diff`'s module doc for the full write-up, including the
//! surprising matching-equality and `new_path` details this module's
//! algorithm alone doesn't explain.
//!
//! # Junk and autojunk
//!
//! `isjunk` is always `None` here — both call sites pass it — so this port
//! never builds a `bjunk` set and `SequenceMatcher`'s junk-specific branches
//! (its `b2j` junk pruning and its two junk-extension loops) are all dead.
//!
//! `autojunk`, by contrast, differs by call site. It is `difflib`'s
//! *default-on* heuristic that treats an element appearing in more than 1%
//! of a ≥200-element sequence as "popular" and drops it from the `b2j`
//! index. `DeepDiff` explicitly opts out for ordered-list comparison
//! (`autojunk=False`; `deepdiff/diff.py::_diff_ordered_iterable_by_difflib`),
//! so [`compute_opcodes`] disables it — confirmed empirically against real
//! `deepdiff==9.1.0` with a 250-item, one-popular-value fixture: the popular
//! value matches exactly like any other. But `_diff_str` diffs two strings
//! with a plain `difflib.unified_diff`, whose `SequenceMatcher(None, a, b)`
//! keeps `difflib`'s default (`autojunk=True`), so [`grouped_opcodes`]
//! enables it; a 250-line string with a popular line genuinely aligns
//! differently, and matching it is what byte-parity on the `diff` field
//! requires. [`build_b2j`] performs the popular-element purge and
//! [`find_longest_match`]'s extension step re-bridges a run across a purged
//! element (the standard-library step this port omits when autojunk is off,
//! where it is provably a no-op — see [`find_longest_match`]).

use std::collections::HashMap;

use crate::value::Value;

/// A bucket key for grouping list elements that compare equal the way
/// Python's `==` (and therefore `difflib`'s matcher, and dict/set hashing)
/// does — **not** the way [`mod@crate::diff`]'s own scalar comparison does.
///
/// This is the "hashable" finding: Python treats `1 == 1.0 == True` (and
/// `0 == 0.0 == False`) as equal regardless of type, so `difflib` can match
/// an `int` in one list against a `float` (or `bool`) of the same numeric
/// value in the other — and, critically, a `difflib` `'equal'` opcode is
/// *never* diffed further (see `crate::diff::array_diff`'s doc), so two
/// cross-type-equal numbers matched this way produce **no** `type_changes`
/// finding at all, unlike every other numeric comparison in this engine
/// (which always treats int/float and bool/int as a type change — see
/// `crate::diff::numbers_equal`'s doc). Confirmed empirically: real
/// `DeepDiff` reports `{}` for `[1]` vs `[1.0]`.
///
/// Integral values (including both bools and float values with no
/// fractional part, within the range a `f64` can represent exactly) are
/// normalized to [`ScalarKey::Int`] so `1`, `1.0`, and `True` all produce the
/// same key; `-0.0` and `0.0` both normalize to `Int(0)`. A float outside
/// that exact range keeps its own [`ScalarKey::Float`] bucket rather than
/// risk a lossy, non-reversible integer cast — an accepted, documented
/// limitation for numbers whose magnitude exceeds `2^53` (real Python
/// itself does exact big-int/float comparison here, which this port does
/// not replicate; see [`MAX_EXACT_F64_INT`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ScalarKey {
    Null,
    Str(String),
    Int(i128),
    /// Bit pattern of a non-integral (or too-large-to-be-exact) float —
    /// hashed through [`mix_float_bits`]; see this type's hand-written `Hash`.
    Float(u64),
    /// A `NaN`, keyed by the address of the [`Value`] node it was read from
    /// — see [`python_scalar_key`]'s `NaN` case for why.
    Nan(usize),
    /// A `datetime`, keyed by whether it is aware and by its instant —
    /// Python's own `datetime.__eq__`/`__hash__` pair, which compares two
    /// aware values by instant (so `10:00+00:00 == 12:00+02:00`) but never
    /// equates a naive value with an aware one, whatever its wall clock.
    /// Deliberately *stricter* than the engine's own datetime comparison,
    /// which reads a naive value as UTC: this key is Python's `==`, which is
    /// what `difflib` matches with.
    DateTime {
        aware: bool,
        instant: i64,
    },
    /// A `date`, keyed by its ordinal. Its own bucket, because a `date` and
    /// a `datetime` are never Python-equal in either direction.
    Date(i64),
    /// A `time`, keyed by whether it is aware and by
    /// [`crate::datetime::Time::sort_instant`] — real `time.__eq__`'s exact
    /// rule (see `crate::datetime`'s module doc): a naive value is never
    /// equal to an aware one, and two aware values compare by an
    /// offset-adjusted instant, at full microsecond precision (unlike the
    /// truncated `ignore_order` hash rule — this key backs `difflib`
    /// matching, which uses plain `==`, not `DeepHash`).
    Time {
        aware: bool,
        instant: i64,
    },
    /// A `timedelta`, keyed by the value itself (already an exact,
    /// `Eq`/`Hash`/`Ord` total-duration type — see
    /// [`crate::datetime::TimeDelta`]'s own doc for why it is not a
    /// flattened microsecond count) — always comparable, with no naive/aware
    /// split.
    TimeDelta(crate::datetime::TimeDelta),
}

/// Avalanche a raw `f64` bit pattern before it is hashed. A float carrying an
/// integer value (`1.0`, `2.0`, …) or a half-integer (`0.5`, `1.5`, …) has ~50
/// trailing zero bits, and `onix`'s hash tables use a fast, weakly-finalising
/// `FxHash`; hashbrown then picks the bucket from the low bits, so a run of
/// such floats would all land in one bucket and every lookup would degrade to a
/// linear scan (`O(n^2)` overall). Folding the high half into the low half and
/// multiplying by a 64-bit odd constant spreads those distinguishing high bits
/// down into the low bits the bucket index reads. Deterministic and injective
/// enough for a hash (equal bit patterns still map to one value, so it stays
/// consistent with `Eq`); it never leaves this module's `Hash` impls.
pub(crate) fn mix_float_bits(bits: u64) -> u64 {
    let mut x = bits ^ (bits >> 32);
    x = x.wrapping_mul(0xd6e8_feb8_6659_fd93);
    x ^= x >> 32;
    x
}

impl std::hash::Hash for ScalarKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        core::mem::discriminant(self).hash(state);
        match self {
            Self::Null => {}
            Self::Str(s) => s.hash(state),
            Self::Int(i) => i.hash(state),
            Self::Float(bits) => mix_float_bits(*bits).hash(state),
            // A `Value` node's address is 8/16-byte-aligned like any other
            // pointer, so its low bits carry no entropy; avalanche it the
            // same way as a trailing-zero-heavy float bit pattern (see
            // `mix_float_bits`'s own doc) rather than trust the raw address.
            // `usize as u64` never truncates: `u64` is at least as wide as
            // `usize` on every target this crate builds for.
            Self::Nan(id) => mix_float_bits(*id as u64).hash(state),
            Self::DateTime { aware, instant } | Self::Time { aware, instant } => {
                aware.hash(state);
                instant.hash(state);
            }
            Self::Date(ordinal) => ordinal.hash(state),
            Self::TimeDelta(value) => value.hash(state),
        }
    }
}

/// The largest magnitude an `f64` can represent every integer up to,
/// exactly (`2^53`). Beyond this, adjacent integers can be indistinguishable
/// in floating point, so [`scalar_key`] does not attempt an exact integer
/// cast past this bound. `onix-arrow`'s row hasher applies the identical bound
/// and fold predicate (`MAX_EXACT_F64_INT` / `hash_float` in
/// `crates/onix-arrow/src/row_diff.rs`); the crates are decoupled, so the two
/// copies move together.
const MAX_EXACT_F64_INT: f64 = 9_007_199_254_740_992.0;

/// Returns `true` if every element of `items` is a JSON scalar (null, bool,
/// number, or string) or a calendar value (datetime, date) —
/// `DeepDiff`'s `_all_values_basic_hashable` check, whose `helper.basic_types`
/// tuple lists `datetime.datetime` and `datetime.date` alongside the numeric
/// and string types.
///
/// A dict, list or tuple anywhere in `items` returns `false`. An empty slice
/// returns `true` (vacuously — matches `DeepDiff`, whose equivalent check is
/// also vacuously true over an empty iterable).
#[must_use]
pub(crate) fn all_basic_scalars(items: &[Value]) -> bool {
    items.iter().all(|item| {
        matches!(
            item,
            Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::Str(_)
                | Value::DateTime(_)
                | Value::Date(_)
                | Value::Time(_)
                | Value::TimeDelta(_)
        )
    })
}

/// Computes `value`'s [`ScalarKey`], for a value already known to be a
/// scalar.
///
/// # Panics
///
/// Panics if `value` is a container. Every caller in this module
/// only ever reaches this after [`all_basic_scalars`] has confirmed the
/// whole slice is scalar-only, so this can never actually fire — see
/// `crate::diff::array_diff`'s dispatch, which gates the entire LCS path on
/// that check. A caller outside that dispatch uses [`python_scalar_key`].
fn scalar_key(value: &Value) -> ScalarKey {
    python_scalar_key(value)
        .expect("scalar_key called on a non-scalar; caller must check all_basic_scalars")
}

/// `value`'s [`ScalarKey`] if it is a scalar, `None` if it is a container.
///
/// This is the crate's single definition of **Python's own `==` on
/// scalars** — the rule that collapses `1`, `1.0` and `True` into one value
/// (see [`ScalarKey`]'s doc for the exact normalization and its `2^53`
/// bound). The ordered-list matcher needs it because `difflib` compares with
/// `==`; `crate::ignore_order` needs the same rule twice more, for
/// `DeepHash`'s cache identity and for the `list(t1) == t2` coercion test,
/// and shares this one rather than restating it.
///
/// A `NaN` gets [`ScalarKey::Nan`], keyed by `value`'s own address, because
/// no bit-pattern-based key could be right here: `NaN != NaN` in Python
/// regardless of bits, confirmed against `deepdiff==9.1.0` — two distinct
/// `NaN` objects with the *same* bits still fail `difflib`'s `==`-based
/// match (`[1, nan_a, 2]` vs `[1, 2, nan_b]` reports a `type_changes` at the
/// `nan`/`2` positions rather than treating the insertion as a clean shift,
/// which a bit-based key that let `nan_a` match `nan_b` would get wrong).
/// Each call therefore has to hand back a key that cannot equal *any* other
/// call's — including another `NaN` read from the exact same bits — and an
/// address is the only per-call-distinct, already-available `usize` this
/// function has: `value` is borrowed from a [`Value`] tree that outlives the
/// whole comparison this key feeds, so the address is stable for exactly as
/// long as the key needs to be looked up, and (being a fresh allocation per
/// converted Python object) two different `NaN` occurrences can never share
/// one. See `tests/golden/README.md`'s "Non-finite floats" section for the
/// one case this cannot reproduce (the *same* Python `NaN` object compared
/// or hashed against itself, which real `DeepDiff`/`CPython` sometimes treat
/// as matching via object identity — a concept this crate's value model
/// does not carry).
pub(crate) fn python_scalar_key(value: &Value) -> Option<ScalarKey> {
    Some(match value {
        Value::Null => ScalarKey::Null,
        Value::Str(s) => ScalarKey::Str(s.to_string()),
        Value::Bool(b) => ScalarKey::Int(i128::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n
                .as_i64()
                .map(i128::from)
                .or_else(|| n.as_u64().map(i128::from))
            {
                return Some(ScalarKey::Int(i));
            }
            let f = n.as_f64().expect("a serde_json Number is i64, u64, or f64");
            if f.is_nan() {
                return Some(ScalarKey::Nan(std::ptr::from_ref(value) as usize));
            }
            if f.fract() == 0.0 && f.abs() <= MAX_EXACT_F64_INT {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "fract() == 0.0 and the magnitude bound above together guarantee an exact round trip"
                )]
                ScalarKey::Int(f as i128)
            } else {
                // `0.0`/`-0.0` are both integral (`fract() == 0.0`), so they
                // are already normalized to `Int(0)` by the branch above —
                // this branch only ever sees a genuinely non-integral (or
                // too-large-to-cast-exactly) float, never needing its own
                // zero-sign normalization.
                ScalarKey::Float(f.to_bits())
            }
        }
        Value::DateTime(value) => ScalarKey::DateTime {
            aware: value.utc_offset_seconds().is_some(),
            instant: value.instant(),
        },
        Value::Date(value) => ScalarKey::Date(value.ordinal()),
        Value::Time(value) => ScalarKey::Time {
            aware: value.utc_offset_seconds().is_some(),
            instant: value.sort_instant(),
        },
        Value::TimeDelta(value) => ScalarKey::TimeDelta(*value),
        Value::Array(_)
        | Value::Tuple(_)
        | Value::Set(_)
        | Value::FrozenSet(_)
        | Value::Object(_) => return None,
    })
}

/// One `difflib`-style edit opcode over `a`/`b` index ranges (half-open,
/// like `difflib.SequenceMatcher.get_opcodes()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Opcode {
    pub(crate) tag: Tag,
    pub(crate) a1: usize,
    pub(crate) a2: usize,
    pub(crate) b1: usize,
    pub(crate) b2: usize,
}

/// The kind of edit an [`Opcode`] describes, matching `difflib`'s own tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tag {
    /// `a[a1..a2]` and `b[b1..b2]` are the same (by [`ScalarKey`] equality),
    /// element for element. Never diffed further — see `crate::diff`'s
    /// module doc.
    Equal,
    /// `a[a1..a2]` should be replaced by `b[b1..b2]`; the two ranges never
    /// share a matching element (see [`compute_opcodes`]'s doc).
    Replace,
    /// `a[a1..a2]` should be deleted (`b1 == b2`).
    Delete,
    /// `b[b1..b2]` should be inserted at `a[a1..a1]` (`a1 == a2`).
    Insert,
}

/// One matching block: `a[a..a+size] == b[b..b+size]` (by
/// [`ScalarKey`] equality).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Match {
    a: usize,
    b: usize,
    size: usize,
}

/// Finds the longest matching block within `a[alo..ahi]` / `b[blo..bhi]` —
/// a port of `difflib.SequenceMatcher.find_longest_match`'s core
/// sparse-DP chaining, with its `isjunk` branches dropped (`isjunk` is
/// always `None` here; see this module's doc). Ties (multiple equal-longest
/// candidates) resolve exactly like `difflib`'s: earliest `i`, then earliest
/// `j` — a direct consequence of scanning `i` ascending and only updating on
/// a *strictly greater* `k`.
///
/// **`difflib`'s post-DP greedy-extension step runs only when `extend` is
/// set** (`while ... and not isbjunk(...): besti -= 1; ...`, run twice —
/// backward then forward). That step exists in the standard library to
/// bridge a match across an *excluded* `b` element (junk, or an
/// autojunk-pruned popular one) that the DP's `b2j` chain skips entirely
/// because such elements are deleted from `b2j` (`__chain_b`). When autojunk
/// is off ([`compute_opcodes`], `extend == false`), `b2j` excludes nothing,
/// so the DP chain alone already finds every genuinely contiguous run the
/// extension step could ever find; re-extending afterwards can only re-derive
/// a run already reflected in some `run_length` the DP computed, so skipping
/// it changes no opcode output — verified by a 400,000-trial randomized
/// differential test against real `difflib` (zero divergences), not only by
/// the argument. When autojunk is on ([`grouped_opcodes`], `extend == true`),
/// [`build_b2j`] *does* purge popular elements, so the step is load-bearing
/// and must run.
///
/// `b2j` maps each of `b`'s [`ScalarKey`]s to the (ascending) list of
/// indices it occurs at in `b`; built once per `a`/`b` pair (see
/// [`build_b2j`]) and reused across every call this function makes during
/// [`get_matching_blocks`]'s traversal, exactly like `difflib`'s own
/// `self.b2j`.
///
/// When `extend` is `true`, `difflib`'s post-DP greedy-extension step runs
/// (it is gated rather than always on — see [`build_b2j`] and
/// [`get_matching_blocks`]). It bridges a match across a `b` element the DP
/// chain skipped because [`build_b2j`] purged it as *popular*: such an
/// element is absent from `b2j`, so the DP never chains through it, yet it
/// still equals its `a` counterpart and `difflib` re-extends the run over
/// it. `difflib`'s `isjunk` is always `None` here (`bjunk` empty), so its
/// two junk-extension loops are dead and only its two non-junk loops matter;
/// with an empty `bjunk`, `not isbjunk(...)` is always true, leaving element
/// equality (by [`ScalarKey`], `difflib`'s `==`) as the sole condition.
///
/// Returns `(best_a, best_b, best_size)`; `best_size == 0` means no match
/// was found in the given range at all.
fn find_longest_match(
    a_keys: &[ScalarKey],
    b_keys: &[ScalarKey],
    window: Window,
    b2j: &HashMap<ScalarKey, Vec<usize>>,
    extend: bool,
) -> (usize, usize, usize) {
    let Window { alo, ahi, blo, bhi } = window;
    let (mut best_a, mut best_b, mut best_size) = (alo, blo, 0);
    let mut run_length_by_b_index: HashMap<usize, usize> = HashMap::new();

    for (offset, key) in a_keys[alo..ahi].iter().enumerate() {
        let a_index = alo + offset;
        let mut next_run_length_by_b_index: HashMap<usize, usize> = HashMap::new();
        if let Some(b_indices) = b2j.get(key) {
            for &b_index in b_indices {
                if b_index < blo {
                    continue;
                }
                if b_index >= bhi {
                    break;
                }
                let run_length = if b_index == 0 {
                    1
                } else {
                    run_length_by_b_index
                        .get(&(b_index - 1))
                        .copied()
                        .unwrap_or(0)
                        + 1
                };
                next_run_length_by_b_index.insert(b_index, run_length);
                if run_length > best_size {
                    best_a = a_index + 1 - run_length;
                    best_b = b_index + 1 - run_length;
                    best_size = run_length;
                }
            }
        }
        run_length_by_b_index = next_run_length_by_b_index;
    }

    if extend {
        while best_a > alo && best_b > blo && a_keys[best_a - 1] == b_keys[best_b - 1] {
            best_a -= 1;
            best_b -= 1;
            best_size += 1;
        }
        while best_a + best_size < ahi
            && best_b + best_size < bhi
            && a_keys[best_a + best_size] == b_keys[best_b + best_size]
        {
            best_size += 1;
        }
    }

    (best_a, best_b, best_size)
}

/// A half-open search window into `a`/`b`, bundling `difflib`'s
/// `alo`/`ahi`/`blo`/`bhi` bounds so [`find_longest_match`] takes them as one
/// argument.
#[derive(Clone, Copy)]
struct Window {
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
}

/// Builds `difflib`'s `b2j` index — each of `b`'s [`ScalarKey`]s mapped to
/// the ascending list of indices it occurs at.
///
/// When `autojunk` is set, this also applies `difflib`'s *autojunk*
/// heuristic (`SequenceMatcher.__chain_b`): for a `b` of 200 or more
/// elements, any element occurring more than `len(b) / 100 + 1` times is
/// "popular" and dropped from the index entirely, so the matcher never
/// chains a run through it (the run is re-bridged instead by
/// [`find_longest_match`]'s extension step). See this module's doc for which
/// call site passes which value. `isjunk` is `None` in both, so no `bjunk`
/// set is ever built.
fn build_b2j(b_keys: &[ScalarKey], autojunk: bool) -> HashMap<ScalarKey, Vec<usize>> {
    let mut b2j: HashMap<ScalarKey, Vec<usize>> = HashMap::new();
    for (b_index, key) in b_keys.iter().enumerate() {
        b2j.entry(key.clone()).or_default().push(b_index);
    }
    if autojunk && b_keys.len() >= 200 {
        let ntest = b_keys.len() / 100 + 1;
        b2j.retain(|_, indices| indices.len() <= ntest);
    }
    b2j
}

/// Returns every non-empty matching block between `a` and `b`, sorted and
/// with adjacent blocks collapsed, terminated by a dummy zero-size block at
/// `(a.len(), b.len())` — a direct, iterative (explicit work-stack, no
/// native recursion) port of `difflib.SequenceMatcher.get_matching_blocks`.
/// `difflib` itself switched to this iterative shape for the same reason
/// this whole engine avoids native recursion on untrusted input: naive
/// recursion here overflowed the stack for some real-world inputs.
///
/// `autojunk` is threaded through to [`build_b2j`] (which decides whether to
/// purge popular elements) and to [`find_longest_match`] (which extends a
/// match over any purged element); the two must agree, so this is the single
/// switch that turns the whole heuristic on for the string-diff path and off
/// for the ordered-list path.
fn get_matching_blocks(a: &[Value], b: &[Value], autojunk: bool) -> Vec<Match> {
    // Compute each element's `ScalarKey` once, up front, rather than
    // recomputing it inside every `find_longest_match` window: for a
    // string-diff `a`/`b` of long lines this turns the worst case's O(N^2)
    // key rebuilds (each cloning the line) into O(N).
    let a_keys: Vec<ScalarKey> = a.iter().map(scalar_key).collect();
    let b_keys: Vec<ScalarKey> = b.iter().map(scalar_key).collect();
    let b2j = build_b2j(&b_keys, autojunk);

    let mut stack = vec![Window {
        alo: 0,
        ahi: a.len(),
        blo: 0,
        bhi: b.len(),
    }];
    let mut raw_matches = Vec::new();
    while let Some(window) = stack.pop() {
        let Window { alo, ahi, blo, bhi } = window;
        let (match_a, match_b, match_size) =
            find_longest_match(&a_keys, &b_keys, window, &b2j, autojunk);
        if match_size > 0 {
            raw_matches.push(Match {
                a: match_a,
                b: match_b,
                size: match_size,
            });
            if alo < match_a && blo < match_b {
                stack.push(Window {
                    alo,
                    ahi: match_a,
                    blo,
                    bhi: match_b,
                });
            }
            if match_a + match_size < ahi && match_b + match_size < bhi {
                stack.push(Window {
                    alo: match_a + match_size,
                    ahi,
                    blo: match_b + match_size,
                    bhi,
                });
            }
        }
    }
    raw_matches.sort_by_key(|m| (m.a, m.b));

    let mut collapsed = Vec::new();
    let (mut pending_a, mut pending_b, mut pending_size) = (0_usize, 0_usize, 0_usize);
    for m in raw_matches {
        if pending_a + pending_size == m.a && pending_b + pending_size == m.b {
            pending_size += m.size;
        } else {
            if pending_size > 0 {
                collapsed.push(Match {
                    a: pending_a,
                    b: pending_b,
                    size: pending_size,
                });
            }
            (pending_a, pending_b, pending_size) = (m.a, m.b, m.size);
        }
    }
    if pending_size > 0 {
        collapsed.push(Match {
            a: pending_a,
            b: pending_b,
            size: pending_size,
        });
    }
    collapsed.push(Match {
        a: a.len(),
        b: b.len(),
        size: 0,
    });
    collapsed
}

/// Computes the `difflib`-style [`Opcode`] list turning `a` into `b`.
///
/// A direct port of `difflib.SequenceMatcher.get_opcodes`. Every gap
/// between two consecutive matching blocks becomes exactly one `Replace`
/// (both sides non-empty), `Delete` (only `a` non-empty), or `Insert` (only
/// `b` non-empty) opcode — never more than one per gap, and, by
/// construction of [`get_matching_blocks`] (which finds *every* match in
/// any given sub-range before giving up on it), **a `Replace` opcode's two
/// ranges never contain a matching element pair at all**, not even a
/// non-contiguous or out-of-position one. Every `'equal'` matching block
/// also becomes an `Opcode` with [`Tag::Equal`].
///
/// # Panics
///
/// Never, for any `a`/`b` for which [`all_basic_scalars`] holds on both —
/// see [`scalar_key`]'s doc for the sole (structurally unreachable here)
/// panic path this function's callees have.
fn opcodes_with(a: &[Value], b: &[Value], autojunk: bool) -> Vec<Opcode> {
    let mut opcodes = Vec::new();
    let (mut i, mut j) = (0_usize, 0_usize);

    for m in get_matching_blocks(a, b, autojunk) {
        let tag = if i < m.a && j < m.b {
            Some(Tag::Replace)
        } else if i < m.a {
            Some(Tag::Delete)
        } else if j < m.b {
            Some(Tag::Insert)
        } else {
            None
        };
        if let Some(tag) = tag {
            opcodes.push(Opcode {
                tag,
                a1: i,
                a2: m.a,
                b1: j,
                b2: m.b,
            });
        }
        (i, j) = (m.a + m.size, m.b + m.size);
        if m.size > 0 {
            opcodes.push(Opcode {
                tag: Tag::Equal,
                a1: m.a,
                a2: i,
                b1: m.b,
                b2: j,
            });
        }
    }

    opcodes
}

/// Computes the ordered-list [`Opcode`]s turning `a` into `b`, with
/// `difflib`'s autojunk heuristic disabled — the exact configuration
/// `DeepDiff` uses for default list comparison (see this module's doc).
#[must_use]
pub(crate) fn compute_opcodes(a: &[Value], b: &[Value]) -> Vec<Opcode> {
    opcodes_with(a, b, false)
}

/// Groups `a`→`b`'s opcodes into change clusters, each with up to `n` lines
/// of surrounding context, dropping the long unchanged stretches between
/// them — a port of `difflib.SequenceMatcher.get_grouped_opcodes`, run with
/// autojunk **on** because it feeds `unified_diff`, which constructs its
/// matcher with `difflib`'s default (`SequenceMatcher(None, a, b)`).
///
/// Each returned inner `Vec` is one contiguous group of opcodes, exactly as
/// `difflib` yields them, and is **never empty**: both sites that emit a
/// group (the long-equal split and the final flush) push at least one opcode
/// into it first. An empty *outer* `Vec` means the two inputs produced no
/// change worth a group (identical, or empty).
#[must_use]
pub(crate) fn grouped_opcodes(a: &[Value], b: &[Value], n: usize) -> Vec<Vec<Opcode>> {
    let mut codes = opcodes_with(a, b, true);
    if codes.is_empty() {
        codes.push(Opcode {
            tag: Tag::Equal,
            a1: 0,
            a2: 1,
            b1: 0,
            b2: 1,
        });
    }

    // Fix up a leading/trailing all-equal opcode so a group never carries
    // more than `n` lines of leading or trailing context.
    if let Some(first) = codes.first_mut()
        && first.tag == Tag::Equal
    {
        first.a1 = first.a1.max(first.a2.saturating_sub(n));
        first.b1 = first.b1.max(first.b2.saturating_sub(n));
    }
    if let Some(last) = codes.last_mut()
        && last.tag == Tag::Equal
    {
        last.a2 = last.a2.min(last.a1 + n);
        last.b2 = last.b2.min(last.b1 + n);
    }

    let nn = n + n;
    let mut groups: Vec<Vec<Opcode>> = Vec::new();
    let mut group: Vec<Opcode> = Vec::new();
    for mut code in codes {
        // A long unchanged run ends the current group and starts the next,
        // keeping only `n` lines of context on either side of the boundary.
        if code.tag == Tag::Equal && code.a2 - code.a1 > nn {
            group.push(Opcode {
                tag: Tag::Equal,
                a1: code.a1,
                a2: code.a2.min(code.a1 + n),
                b1: code.b1,
                b2: code.b2.min(code.b1 + n),
            });
            groups.push(std::mem::take(&mut group));
            code.a1 = code.a1.max(code.a2.saturating_sub(n));
            code.b1 = code.b1.max(code.b2.saturating_sub(n));
        }
        group.push(code);
    }
    let trivial_equal = matches!(group.as_slice(), [only] if only.tag == Tag::Equal);
    if !group.is_empty() && !trivial_equal {
        groups.push(group);
    }

    groups
}

#[cfg(test)]
#[path = "lcs_tests.rs"]
mod tests;
