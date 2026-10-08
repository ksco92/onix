.PHONY: check stack-check fmt clippy test test-all-features coverage docs deny machete mutants bench python-test

# Everything must be green; CI runs this target.
check: fmt clippy test test-all-features coverage docs deny machete

fmt:
	cargo fmt --all --check

clippy:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test --workspace

# Runs the suite with every feature on so the `onix-arrow/profile` arm of the
# row diff (the pass!/accum! boundaries) and the profiler's own tests execute;
# the default `test` and `coverage` targets build default features only.
test-all-features:
	cargo test --workspace --all-features

# Coverage scope and the exclusion of onix-py: CONTRIBUTING.md, Coverage scope.
coverage:
	cargo llvm-cov --workspace --fail-under-lines 95 --ignore-filename-regex 'crates/onix-py/src/'

docs:
	RUSTDOCFLAGS="-D warnings" cargo doc --document-private-items --no-deps --workspace --quiet

deny:
	cargo deny check

machete:
	cargo machete

# Mutation testing: slow by design, run periodically, not on every check.
# Scope and rationale: CONTRIBUTING.md, Coverage scope and Mutation testing.
mutants:
	@command -v cargo-mutants >/dev/null 2>&1 || { echo "cargo-mutants not installed: cargo install cargo-mutants --locked"; exit 1; }
	cargo mutants --package onix-core --package onix-cli --package onix-arrow

# Fails when a shape's debug stack cost exceeds PER_LEVEL_STACK_BYTES in crates/onix-py/src/guard.rs.
stack-check:
	cargo run --quiet -p onix-core --example stack_frame_cost -- --max-bytes-per-level 8192

# Regenerates perf/RESULTS.md from a real run against pinned deepdiff.
# Slow (tens of minutes on the full fixture matrix, including two
# multi-second-per-diff "very heavy" fixtures) — see perf/run_bench.sh and
# CONTRIBUTING.md's "Benchmarking" section.
bench:
	@command -v hyperfine >/dev/null 2>&1 || { echo "hyperfine not installed: brew install hyperfine (or: cargo install hyperfine)"; exit 1; }
	perf/run_bench.sh

# The pytest suite for crates/onix-py. Not part of `check`: it needs a Python venv (uv)
# and a release build of the extension module, which the Rust-only check does not set up.
python-test:
	@command -v uv >/dev/null 2>&1 || { echo "uv not installed: see https://docs.astral.sh/uv/"; exit 1; }
	@command -v maturin >/dev/null 2>&1 || { echo "maturin not installed: uv tool install maturin"; exit 1; }
	cd crates/onix-py && uv sync --group test
	cd crates/onix-py && uv run --group test maturin develop --release
	# `--no-sync`: the env is already synced above and `maturin develop` has just
	# installed the freshly built extension; a re-sync here would reinstall the
	# package from uv's build cache (keyed on the dynamic version, not the source),
	# silently replacing that fresh build with a stale one.
	cd crates/onix-py && uv run --no-sync --group test pytest tests -q
