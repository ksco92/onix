.PHONY: check fmt clippy test coverage docs deny machete mutants bench

# The local merge gate until CI exists: everything must be green.
check: fmt clippy test coverage docs deny machete

fmt:
	cargo fmt --all --check

clippy:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test --workspace

# M5a gave onix-cli real logic plus its own unit + integration tests, so it
# is now held to the same coverage bar as the rest of the workspace — no
# exclusion.
#
# Known accepted artifact (M7b): a #[cfg(test)] #[path = "..."] mod tests;
# file (e.g. diff/tests.rs, ignore_order/tests.rs, lcs_tests.rs,
# report_tests.rs, onix-cli/src/tests.rs) does not appear as its own row in
# cargo-llvm-cov's report, and its regions/lines are not folded into any
# other file's totals either — they simply vanish from both the numerator
# and denominator. This moves the percentage (a smaller denominator with
# the same absolute miss count reads as a lower percentage) without any
# change in what's actually tested — confirmed by diffing absolute Missed
# Regions/Lines/Functions counts before/after the M7b split (27/10/2, both
# times). The real, practical effect: a dead/unreachable branch *inside a
# test helper function itself* in one of these files is no longer caught by
# this 95% gate (branches in the production code they test still are,
# since that code lives in an ordinary same-crate file). Accepted, not
# fixed — the alternative (keeping every test module inline in its
# production file) is the exact one-screenful-of-mental-model problem M7b
# addressed.
coverage:
	cargo llvm-cov --workspace --fail-under-lines 95

docs:
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --quiet

deny:
	cargo deny check

machete:
	cargo machete

# Mutation testing: coverage's honest sibling. Slow by design — run per
# milestone (M4+), not on every check. Install: cargo install cargo-mutants
#
# --workspace: both onix-core and onix-cli now carry real logic and their
# own tests (onix-cli gained its `diff` subcommand at M5a, with the
# coverage exclusion removed from `make coverage` above), so both are
# mutation-tested. Every finding (caught, missed, or unviable) is triaged:
# a missed mutant either gets a new regression test that kills it, or, if
# justified, a recorded explanation of why it's equivalent/non-actionable.
mutants:
	@command -v cargo-mutants >/dev/null 2>&1 || { echo "cargo-mutants not installed: cargo install cargo-mutants --locked"; exit 1; }
	cargo mutants --workspace

# M6: regenerates perf/RESULTS.md from a real run against pinned deepdiff.
# Slow (tens of minutes on the full fixture matrix, including two
# multi-second-per-diff "very heavy" fixtures) — see perf/run_bench.sh and
# README.md's "Benchmarks" section.
bench:
	@command -v hyperfine >/dev/null 2>&1 || { echo "hyperfine not installed: brew install hyperfine (or: cargo install hyperfine)"; exit 1; }
	perf/run_bench.sh
