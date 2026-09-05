.PHONY: check fmt clippy test coverage docs deny machete mutants bench python-test

# The local merge gate until CI exists: everything must be green.
check: fmt clippy test coverage docs deny machete

fmt:
	cargo fmt --all --check

clippy:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test --workspace

# onix-cli is held to the same coverage bar as the rest of the workspace:
# its diff subcommand logic has unit + integration tests, so it is not
# excluded.
#
# Coverage-tooling quirk: a #[cfg(test)] #[path = "..."] mod tests;
# file (e.g. diff/tests.rs, ignore_order/tests.rs, lcs_tests.rs,
# report_tests.rs, onix-cli/src/tests.rs) does not appear as its own row in
# cargo-llvm-cov's report, and its regions/lines are not folded into any
# other file's totals either — they simply vanish from both the numerator
# and denominator. That shrinks the denominator (the same absolute miss
# count then reads as a lower percentage) without any change in what's
# actually tested: the absolute Missed Regions/Lines/Functions counts are
# identical whether or not these modules are split out (27/10/2). The real,
# practical effect: a dead/unreachable branch *inside a test helper function
# itself* in one of these files is no longer caught by this 95% gate
# (branches in the production code they test still are, since that code
# lives in an ordinary same-crate file). This is accepted: keeping every
# test module inline in its production file to avoid it would reintroduce
# the one-screenful-of-mental-model problem the split solves.
# onix-py (the PyO3 bindings crate) is excluded from the line-coverage
# denominator: it's a cdylib whose entire surface is Python-object
# conversion and PyO3 glue code — cargo-llvm-cov instruments it fine, but
# every branch that actually matters (which Python type an object is, an
# out-of-range int, a non-str dict key, …) is only meaningfully exercised
# by calling the *compiled wheel* from real Python, which the Rust-side
# `cargo test` harness cannot do (see crates/onix-py/Cargo.toml's
# `extension-module` feature doc for why the crate builds two different
# ways for `cargo test` vs. a Python-loadable wheel). Its coverage
# authority is instead `make python-test` (CONTRIBUTING.md's "Python
# bindings" section):
# a structural exclusion, not a logic-free-shim one.
coverage:
	cargo llvm-cov --workspace --fail-under-lines 95 --ignore-filename-regex 'crates/onix-py/src/'

docs:
	RUSTDOCFLAGS="-D warnings" cargo doc --document-private-items --no-deps --workspace --quiet

deny:
	cargo deny check

machete:
	cargo machete

# Mutation testing: coverage's honest sibling. Slow by design — run
# periodically, not on every check. Install: cargo install cargo-mutants
#
# Scoped to onix-core and onix-cli, the same two crates coverage holds
# to the 95% bar. onix-py is excluded for the same structural reason it is
# excluded from coverage: it is a cdylib whose logic is only exercised by
# calling the compiled wheel from Python, which the Rust test harness cannot
# do, so every onix-py mutant would survive vacuously. This scope matches the
# reproduce command recorded in perf/MUTANTS.md, where every finding is triaged.
mutants:
	@command -v cargo-mutants >/dev/null 2>&1 || { echo "cargo-mutants not installed: cargo install cargo-mutants --locked"; exit 1; }
	cargo mutants --package onix-core --package onix-cli

# Regenerates perf/RESULTS.md from a real run against pinned deepdiff.
# Slow (tens of minutes on the full fixture matrix, including two
# multi-second-per-diff "very heavy" fixtures) — see perf/run_bench.sh and
# CONTRIBUTING.md's "Benchmarking" section.
bench:
	@command -v hyperfine >/dev/null 2>&1 || { echo "hyperfine not installed: brew install hyperfine (or: cargo install hyperfine)"; exit 1; }
	perf/run_bench.sh

# The real product validation for crates/onix-py (its coverage
# authority — see the `coverage` target's comment above). Not part of
# `check`: it needs a Python venv (uv) and a release build of the extension
# module, neither of which the Rust-only gate sets up. See CONTRIBUTING.md's
# "Python bindings" section for the manual equivalent step by step.
python-test:
	@command -v uv >/dev/null 2>&1 || { echo "uv not installed: see https://docs.astral.sh/uv/"; exit 1; }
	@command -v maturin >/dev/null 2>&1 || { echo "maturin not installed: uv tool install maturin"; exit 1; }
	cd crates/onix-py && uv sync --group test
	cd crates/onix-py && uv run --group test maturin develop --release
	cd crates/onix-py && uv run --group test pytest tests -q
