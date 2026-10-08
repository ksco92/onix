// Portions of this module reimplement algorithms from CPython 3.14.6's
// `difflib` standard-library module (`SequenceMatcher` and its autojunk
// heuristic), used under the PSF License Agreement version 2. See
// THIRD-PARTY-NOTICES.md at the repository root.
//! Port of Python's `difflib.SequenceMatcher` opcode algorithm over scalar [`Value`] slices, with
//! `isjunk` always `None` and autojunk off ([`compute_opcodes`], `DeepDiff`'s ordered-list
//! comparison) or on ([`grouped_opcodes`], the `unified_diff` behind [`mod@crate::unified_diff`]).
//! `docs/design/list-diff.md` has the list-compat rules.

use std::collections::HashMap;

use num_bigint::BigInt;
use num_traits::{FromPrimitive, ToPrimitive};

use crate::value::Value;

/// Bucket key for elements equal under Python's `==`, not [`mod@crate::diff`]'s scalar comparison:
/// `1`, `1.0` and `True` share [`ScalarKey::Int`], so `difflib` matches them and reports no
/// `type_changes`. Integral floats beyond `2^53` keep a `Float` bucket, so Python's exact
/// big-int/float comparison is not reproduced there.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ScalarKey {
    Null,
    /// WTF-8 bytes, so a lone surrogate compares correctly.
    Str(Vec<u8>),
    Int(i128),
    /// An integer beyond `i128` that no `f64` represents exactly; boxed to keep the enum small.
    Big(Box<BigInt>),
    /// Bit pattern of a non-integral or beyond-`2^53` float, hashed through [`mix_float_bits`].
    Float(u64),
    /// A `NaN`, keyed by its `Value` node's address so no two ever match (`NaN != NaN`). `difflib`
    /// matches one repeated `NaN` object to itself; onix never does (`tests/golden/README.md`).
    Nan(usize),
    /// Aware flag plus instant: aware values compare by instant, a naive one never equals them.
    DateTime {
        aware: bool,
        instant: i64,
    },
    Date(i64),
    /// Aware flag plus [`crate::datetime::Time::sort_instant`], at full microsecond precision.
    Time {
        aware: bool,
        instant: i64,
    },
    TimeDelta(crate::datetime::TimeDelta),
}

/// Avalanches a float's bit pattern before hashing: integral and half-integer floats share about
/// 50 trailing zero bits, so under `FxHash` they would pile into one bucket (`O(n^2)` lookups).
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
            Self::Big(b) => b.hash(state),
            Self::Float(bits) => mix_float_bits(*bits).hash(state),
            // A `Value` address has no entropy in its low bits; avalanche it like a float.
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

/// `2^53`: past it an `f64` cannot represent every integer, so [`python_scalar_key`] stops casting
/// integral floats. `onix-arrow`'s `row_diff.rs` mirrors this bound; change both.
const MAX_EXACT_F64_INT: f64 = 9_007_199_254_740_992.0;

/// Returns `true` if every element of `items` is a JSON scalar or a calendar
/// value — `DeepDiff`'s `_all_values_basic_hashable` check over
/// `helper.basic_types`.
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

/// [`python_scalar_key`] for a value [`all_basic_scalars`] has already admitted.
fn scalar_key(value: &Value) -> ScalarKey {
    python_scalar_key(value)
        .expect("scalar_key called on a non-scalar; caller must check all_basic_scalars")
}

/// `value`'s [`ScalarKey`], or `None` for a container: the crate's one definition of Python `==`
/// on scalars, shared with `crate::ignore_order` (`DeepHash` identity, the `list(t1) == t2` test).
pub(crate) fn python_scalar_key(value: &Value) -> Option<ScalarKey> {
    Some(match value {
        Value::Null => ScalarKey::Null,
        Value::Str(s) => ScalarKey::Str(s.as_bytes().to_vec()),
        Value::Bool(b) => ScalarKey::Int(i128::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n
                .as_i64()
                .map(i128::from)
                .or_else(|| n.as_u64().map(i128::from))
            {
                return Some(ScalarKey::Int(i));
            }
            if let Some(big) = n.as_big() {
                // A big int that round-trips through `f64` shares that float's key
                // (`10**20 == 1e20`); any other keeps its own `Big` key.
                let as_float = big.to_f64().unwrap_or(f64::INFINITY);
                if as_float.is_finite()
                    && BigInt::from_f64(as_float).is_some_and(|rounded| &rounded == big)
                {
                    return Some(ScalarKey::Float(as_float.to_bits()));
                }
                return Some(ScalarKey::Big(Box::new(big.clone())));
            }
            let f = n.as_f64().expect("a non-integer Number is a float");
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
        Value::TimeDelta(value) => ScalarKey::TimeDelta(value.value()),
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
    /// element for element. Never diffed further — see
    /// `docs/design/list-diff.md`.
    Equal,
    /// `a[a1..a2]` should be replaced by `b[b1..b2]`; the two ranges never
    /// share a matching element (see [`opcodes_with`]'s doc).
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

/// Longest matching block in `window`, ported from `difflib`'s DP: ties go to the earliest `i`,
/// then the earliest `j`. `extend` runs `difflib`'s post-DP extension over elements `b2j` purged as
/// popular; with autojunk off `b2j` excludes nothing, so the step is a no-op and is skipped.
/// Returns `(best_a, best_b, best_size)`; size 0 means no match.
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

/// `difflib`'s `b2j`: each key of `b` mapped to its ascending indices. With `autojunk` and
/// `b.len() >= 200`, keys occurring more than `b.len() / 100 + 1` times are dropped as popular.
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

/// Non-empty matching blocks, sorted and merged when adjacent, ended by a zero-size block at
/// `(a.len(), b.len())`. Uses an explicit work-stack, not native recursion; `autojunk` must reach
/// both [`build_b2j`] and [`find_longest_match`].
fn get_matching_blocks(a: &[Value], b: &[Value], autojunk: bool) -> Vec<Match> {
    // Keys are built once, not per window, so long string lines are not re-cloned per call.
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

/// `difflib`'s `get_opcodes`: one `Replace`, `Delete` or `Insert` per gap between matching blocks
/// plus an `Equal` per block; a `Replace`'s two ranges share no matching element.
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

/// Opcodes turning `a` into `b`, autojunk off.
#[must_use]
pub(crate) fn compute_opcodes(a: &[Value], b: &[Value]) -> Vec<Opcode> {
    opcodes_with(a, b, false)
}

/// `difflib`'s `get_grouped_opcodes`: change clusters with up to `n` lines of context, run with
/// autojunk on as `unified_diff` requires. Inner groups are never empty; the outer `Vec` is empty
/// when nothing changed.
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
