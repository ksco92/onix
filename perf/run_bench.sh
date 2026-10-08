#!/usr/bin/env bash
# Benchmark driver: onix vs. real DeepDiff, across the full fixture
# matrix (every fixture is a real two-tool comparison — see
# `extra_diff_flags_for` below for `ignore_order_10k`'s `--ignore-order`).
#
# Deterministic: fixed warmup/run counts per fixture (see tier_for below),
# the same command sequence every run, and every correctness precondition
# checked before any number is trusted.
#
# Usage: perf/run_bench.sh
#
# Raw per-run JSON lands in perf/bench_raw/ (gitignored — intermediate
# machine-specific data; only RESULTS.md is committed).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

export PATH="$HOME/.cargo/bin:$PATH"

FIXTURES_DIR="$ROOT/perf/fixtures"
RAW_DIR="$ROOT/perf/bench_raw"
BIN="$ROOT/target/release/onix"
MAX_DEPTH=512

log() { echo "[run_bench] $*"; }

# Extra CLI flags a fixture's diff needs, on BOTH tools (onix and
# `run_deepdiff.py` both spell it `--ignore-order`) — empty for every
# fixture except `ignore_order_10k` (the `ignore_order=True` headline
# comparison). `ignore_order_10k` is an all-numeric flat list, so it never
# reaches the one case `KNOWN_DIVERGENT_CASES` holds in
# `crates/onix-core/tests/golden.rs` (`path_rendering_collision`)
# — the correctness precheck below applies to it exactly like every other
# fixture, with no special-casing.
extra_diff_flags_for() {
  case "$1" in
    ignore_order_10k) echo "--ignore-order" ;;
    *) echo "" ;;
  esac
}

# Echoes "WARMUP RUNS" per fixture; the tiers are in RESULTS.md's run-procedure table.
# A plain case statement, not `declare -A` (associative arrays): macOS
# ships bash 3.2 as `/bin/bash` (associative arrays need bash 4+), and this
# script must run with no extra tooling beyond what README.md already
# documents as prerequisites.
tier_for() {
  case "$1" in
    flat_dict_10k | flat_dict_100k | flat_list_100k | deep_narrow_d120) echo "3 10" ;;
    startup_trivial) echo "5 20" ;;
    flat_dict_1m | identical_1m | ignore_order_10k) echo "1 5" ;;
    nested_uniform_d6_b10 | api_payloads) echo "0 3" ;;
    *)
      echo "tier_for: unknown fixture '$1'" >&2
      exit 1
      ;;
  esac
}

##############################################
##############################################
##############################################
##############################################
# Step 1: fixtures

if [ ! -f "$FIXTURES_DIR/manifest.json" ]; then
  log "Step 1/8: generating fixtures (perf/fixtures/ absent)"
  uv run "$ROOT/perf/generate_fixtures.py"
else
  log "Step 1/8: perf/fixtures/ already present — skipping regeneration (rm -rf perf/fixtures to force)"
fi

# Every fixture in the manifest is diffed by both tools.
# shellcheck disable=SC2207  # mapfile/read -a need bash 4+; fixture names are
# plain identifiers (no spaces/globs), so word-splitting here is safe.
FIXTURES=($(jq -r '.fixtures[].name' "$FIXTURES_DIR/manifest.json"))

##############################################
##############################################
##############################################
##############################################
# Step 2: build

log "Step 2/8: cargo build --release"
cargo build --release --quiet

# Warm uv's cached environment for run_deepdiff.py once, up front: uv prints
# an "Installed N packages" line to stderr on the FIRST invocation only,
# which would otherwise corrupt the very first self-instrumentation JSON
# line this script parses from stderr.
uv run "$ROOT/perf/run_deepdiff.py" --help >/dev/null 2>&1 || true

##############################################
##############################################
##############################################
##############################################
# Step 3: environment header

log "Step 3/8: recording environment"
rm -rf "$RAW_DIR"
mkdir -p "$RAW_DIR"

PYTHON_VERSION="$(uv run --python 3.13 python3 --version 2>/dev/null | tr -d '\n')"

cat > "$RAW_DIR/env.json" <<EOF
{
  "date_utc": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "os": "$(sw_vers -productName) $(sw_vers -productVersion) (build $(sw_vers -buildVersion))",
  "cpu": "$(sysctl -n machdep.cpu.brand_string)",
  "cores": $(sysctl -n hw.ncpu),
  "memory_bytes": $(sysctl -n hw.memsize),
  "rustc_version": "$(rustc -V)",
  "cargo_version": "$(cargo -V)",
  "uv_version": "$(uv --version)",
  "hyperfine_version": "$(hyperfine --version)",
  "python_version": "$PYTHON_VERSION",
  "deepdiff_version": "9.1.0"
}
EOF

##############################################
##############################################
##############################################
##############################################
# Step 4: correctness precheck
#
# One run per tool per fixture: stdout is canonicalized (jq -S) and
# compared. Diff-only timing is not read from these runs (see Step 5).

precheck_onix() {
  local fixture="$1"
  local extra
  extra="$(extra_diff_flags_for "$fixture")"
  # shellcheck disable=SC2086  # $extra is a fixed, controlled flag string
  # (empty or "--ignore-order"); word-splitting it is intentional here.
  "$BIN" diff "$FIXTURES_DIR/$fixture/a.json" "$FIXTURES_DIR/$fixture/b.json" \
    --max-depth "$MAX_DEPTH" $extra \
    >"$RAW_DIR/onix_stdout_$fixture.json" \
    2>/dev/null
  jq -S . <"$RAW_DIR/onix_stdout_$fixture.json" >"$RAW_DIR/onix_canon_$fixture.json"
}

precheck_deepdiff() {
  local fixture="$1"
  local extra
  extra="$(extra_diff_flags_for "$fixture")"
  # shellcheck disable=SC2086  # see precheck_onix's identical justification.
  uv run "$ROOT/perf/run_deepdiff.py" "$FIXTURES_DIR/$fixture/a.json" "$FIXTURES_DIR/$fixture/b.json" $extra \
    >"$RAW_DIR/deepdiff_stdout_$fixture.json" \
    2>/dev/null
  jq -S . <"$RAW_DIR/deepdiff_stdout_$fixture.json" >"$RAW_DIR/deepdiff_canon_$fixture.json"
}

log "Step 4/8: correctness precheck (onix vs. real DeepDiff, canonical JSON)"
for f in "${FIXTURES[@]}"; do
  log "  precheck: $f"
  precheck_onix "$f"
  precheck_deepdiff "$f"

  if ! diff -q "$RAW_DIR/onix_canon_$f.json" "$RAW_DIR/deepdiff_canon_$f.json" >/dev/null; then
    echo "[run_bench] CORRECTNESS MISMATCH on fixture '$f' — aborting the whole run." >&2
    echo "[run_bench] A perf number on divergent output is void." >&2
    diff "$RAW_DIR/onix_canon_$f.json" "$RAW_DIR/deepdiff_canon_$f.json" | head -40 >&2
    exit 1
  fi

  log "    MATCH"
done

##############################################
##############################################
##############################################
##############################################
# Step 5: diff-only timing sampling
#
# Its own N-sample loop with the tier's warmup/run counts; warmup runs are
# discarded, only the measured runs are written to the samples file.

sample_diff_only_onix() {
  local fixture="$1" warmup="$2" runs="$3"
  local a="$FIXTURES_DIR/$fixture/a.json"
  local b="$FIXTURES_DIR/$fixture/b.json"
  local extra
  extra="$(extra_diff_flags_for "$fixture")"
  local scratch
  scratch="$(mktemp)"

  local i
  for ((i = 0; i < warmup; i++)); do
    # shellcheck disable=SC2086  # see precheck_onix's identical justification.
    "$BIN" diff "$a" "$b" --max-depth "$MAX_DEPTH" $extra --timing >/dev/null 2>/dev/null
  done

  : >"$scratch"

  for ((i = 0; i < runs; i++)); do
    # shellcheck disable=SC2086  # see precheck_onix's identical justification.
    "$BIN" diff "$a" "$b" --max-depth "$MAX_DEPTH" $extra --timing >/dev/null 2>>"$scratch"
  done

  jq -s '{diff_ns_samples: [.[].diff_ns], parse_ns_samples: [.[].parse_ns]}' "$scratch" \
    >"$RAW_DIR/diffonly_onix_$fixture.json"
  rm -f "$scratch"
}

sample_diff_only_deepdiff() {
  local fixture="$1" warmup="$2" runs="$3"
  local a="$FIXTURES_DIR/$fixture/a.json"
  local b="$FIXTURES_DIR/$fixture/b.json"
  local extra
  extra="$(extra_diff_flags_for "$fixture")"
  local scratch
  scratch="$(mktemp)"

  local i
  for ((i = 0; i < warmup; i++)); do
    # shellcheck disable=SC2086  # see precheck_onix's identical justification.
    uv run "$ROOT/perf/run_deepdiff.py" "$a" "$b" $extra >/dev/null 2>/dev/null
  done

  : >"$scratch"

  for ((i = 0; i < runs; i++)); do
    # shellcheck disable=SC2086  # see precheck_onix's identical justification.
    uv run "$ROOT/perf/run_deepdiff.py" "$a" "$b" $extra >/dev/null 2>>"$scratch"
  done

  jq -s '{
    diff_ns_samples: [.[].diff_ns],
    parse_ns_samples: [.[].parse_ns],
    tracemalloc_peak_bytes_samples: [.[].tracemalloc_peak_bytes],
    ru_maxrss_before_bytes_samples: [.[].ru_maxrss_before_bytes],
    ru_maxrss_after_bytes_samples: [.[].ru_maxrss_after_bytes],
    ru_maxrss_delta_bytes_samples: [.[].ru_maxrss_delta_bytes]
  }' "$scratch" >"$RAW_DIR/diffonly_deepdiff_$fixture.json"
  rm -f "$scratch"
}

log "Step 5/8: diff-only timing sampling (N tier-appropriate runs per fixture)"
for f in "${FIXTURES[@]}"; do
  read -r warmup runs <<<"$(tier_for "$f")"
  log "  sampling: $f (warmup=$warmup, runs=$runs)"
  sample_diff_only_onix "$f" "$warmup" "$runs"
  sample_diff_only_deepdiff "$f" "$warmup" "$runs"
done

##############################################
##############################################
##############################################
##############################################
# Step 6: hyperfine sweep (wall clock + CPU time + peak RSS in one pass)
#
# hyperfine's --export-json reports, per command, mean/median/stddev wall
# time, mean user+system CPU time, AND per-run memory_usage_byte — covering
# wall clock, CPU time, and peak RSS in one sweep.

run_hyperfine() {
  local warmup="$1" runs="$2" export_json="$3"
  shift 3
  hyperfine \
    --warmup "$warmup" --runs "$runs" \
    --export-json "$export_json" \
    "$@" \
    >"${export_json%.json}_stdout.txt" 2>&1
}

log "Step 6/8: hyperfine sweep"
for f in "${FIXTURES[@]}"; do
  read -r warmup runs <<<"$(tier_for "$f")"
  log "  hyperfine: $f (warmup=$warmup, runs=$runs)"
  a="$FIXTURES_DIR/$f/a.json"
  b="$FIXTURES_DIR/$f/b.json"
  extra="$(extra_diff_flags_for "$f")"
  run_hyperfine "$warmup" "$runs" "$RAW_DIR/hyperfine_$f.json" \
    --command-name onix "$BIN diff $a $b --max-depth $MAX_DEPTH $extra >/dev/null" \
    --command-name deepdiff "uv run $ROOT/perf/run_deepdiff.py $a $b $extra >/dev/null"
done

##############################################
##############################################
##############################################
##############################################
# Step 7: energy (best-effort)
#
# macOS powermetrics needs root. Only attempted if `sudo` works
# non-interactively (`sudo -n`) — the common case is that it doesn't
# (headless/CI/sandboxed environments), in which case this degrades
# gracefully to the documented CPU-seconds proxy (already captured by
# hyperfine's user+system columns above) rather than failing the run.

log "Step 7/8: energy sampling (best-effort)"
SUDO_POWERMETRICS_CMD=(sudo powermetrics --samplers cpu_power -i 200 -n 25 --show-process-energy)
if sudo -n true 2>/dev/null; then
  log "  sudo available non-interactively — sampling powermetrics over a small onix+deepdiff loop"
  ENERGY_AVAILABLE=true
  a="$FIXTURES_DIR/flat_dict_100k/a.json"
  b="$FIXTURES_DIR/flat_dict_100k/b.json"
  "${SUDO_POWERMETRICS_CMD[@]}" 2>&1 | tee "$RAW_DIR/powermetrics_onix.txt" >/dev/null &
  PM_PID=$!
  for _ in $(seq 1 25); do "$BIN" diff "$a" "$b" --max-depth "$MAX_DEPTH" >/dev/null; done
  wait "$PM_PID" || true
else
  log "  sudo requires a password in this sandboxed environment — skipping; CPU-seconds (hyperfine's user+system columns, Step 6) is the documented proxy."
  ENERGY_AVAILABLE=false
fi

cat > "$RAW_DIR/energy.json" <<EOF
{"available": $ENERGY_AVAILABLE, "manual_sudo_command": "${SUDO_POWERMETRICS_CMD[*]}"}
EOF

##############################################
##############################################
##############################################
##############################################
# Step 8: write perf/RESULTS.md

log "Step 8/8: writing perf/RESULTS.md"
uv run "$ROOT/perf/summarize_results.py"

log "Done — see perf/RESULTS.md"
