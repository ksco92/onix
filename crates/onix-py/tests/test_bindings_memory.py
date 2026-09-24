"""Peak-RSS regression guard for the direct compact-value build.

The bindings convert both inputs into `onix_core::Value` directly, with no
intermediate `serde_json::Value` tree. Measures the incremental process RSS
of converting an ``api_payloads``-shaped fixture and asserts it stays under
a bound the old `serde_json::Value` intermediate would blow past.

Run in its own subprocess (isolates the RSS reading from pytest), via
``ru_maxrss`` normalized across macOS (bytes) and Linux (KiB).
"""

import json
import subprocess
import sys
import textwrap
from pathlib import Path

# Regression bound: comfortably above the compact build's measured overhead, so allocator/platform noise can't flake it.
_OVERHEAD_BOUND_MB = 90.0
_RECORD_COUNT = 10_000

# The benchmark fixture generator lives in perf/ (repo root, three levels up
# from this tests/ directory); the subprocess adds it to sys.path and imports
# `build_api_payloads` so the record shape is single-sourced there.
_PERF_DIR = Path(__file__).resolve().parents[3] / "perf"

_SUBPROCESS_BODY = """
import json
import resource
import sys

sys.path.insert(0, "__PERF_DIR__")
from generate_fixtures import build_api_payloads

from deepdiff_rs import DeepDiff


def rss_mb():
    # macOS reports ru_maxrss in bytes; Linux in KiB.
    factor = 1 if sys.platform == "darwin" else 1024
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * factor / 1e6


# Only the first element of the (a, b) pair is needed: diffing it against
# itself gives an empty report, so the measured RSS delta is exactly the two
# compact input trees the conversion builds and holds during the diff -- the
# pure conversion overhead, with no report contribution.
payload = build_api_payloads(__RECORD_COUNT__, 1234)[0]

before = rss_mb()
diff = DeepDiff(payload, payload)
after = rss_mb()
assert not bool(diff), "equal inputs must produce an empty report"

print(json.dumps({"overhead_mb": after - before, "records": __RECORD_COUNT__}))
"""


def test_api_payloads_conversion_overhead_is_bounded() -> None:
    body = (
        textwrap.dedent(_SUBPROCESS_BODY)
        .replace("__PERF_DIR__", str(_PERF_DIR))
        .replace("__RECORD_COUNT__", str(_RECORD_COUNT))
    )
    result = subprocess.run(
        [sys.executable, "-c", body],
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    assert result.returncode == 0, result.stderr

    data = json.loads(result.stdout.strip().splitlines()[-1])
    overhead = data["overhead_mb"]
    assert overhead < _OVERHEAD_BOUND_MB, (
        f"compact conversion overhead {overhead:.1f} MB for {data['records']} "
        f"api_payloads-shaped records exceeds the {_OVERHEAD_BOUND_MB} MB bound "
        f"(a regression to the serde_json::Value intermediate would land here)"
    )
